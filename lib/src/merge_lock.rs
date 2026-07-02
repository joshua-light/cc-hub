//! Project-level merge lock — at most one task per project may be in the
//! `Merging` phase at any moment. The PR-flow design serializes merges on
//! purpose: each merging task fetches main, resolves conflicts against
//! whatever just landed, and then merges into a known-stable target. With
//! serialization the conflict-resolution step can never see the target
//! shifting under it.
//!
//! The lock lives at `~/.cc-hub/projects/<pid>/merge.lock` and contains a
//! JSON record naming the holder. Every mutation (acquire, refresh, steal,
//! phase/prior_ref update, release) runs under an exclusive advisory flock on
//! a sidecar `merge.guard` file — the same fs2 pattern the per-task
//! `state.lock` uses — so read-decide-write sequences can't interleave across
//! processes. The guard is held only for the milliseconds of the mutation,
//! never across the Merging phase itself; the merge.lock *record* is what
//! persists (and what stale detection reasons about), while the advisory
//! guard auto-releases on process death and therefore can't go stale itself.
//! The record is always written via tempfile+rename, so unguarded readers
//! never observe a partial file.
//!
//! Stale-lock detection: if the holder's tmux session is gone, the lock is
//! treated as released and a fresh acquisition is allowed. This rescues
//! tasks where the orchestrator died between merge and finalize. A hard TTL
//! (`STALE_TTL_SECS`) backs that up: even if tmux somehow lingers, a lock
//! older than the TTL is forfeit.
//!
//! The lock spans the entire Merging phase: from `cc-hub pr merge` through
//! `/simplify` + `/bump` to `cc-hub pr finalize`. /simplify and /bump touch
//! `main` directly (Cargo.toml, lockfiles), so they must inherit the same
//! exclusion the merge itself enforced.
//!
//! The lock also tracks the current sub-phase (merging / simplify / bump /
//! finalize_pending) so the renderer can show queued tasks what the holder
//! is doing.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;

use crate::orchestrator;

/// How long a lock can live before another task may steal it, regardless
/// of whether the holder's tmux session is still alive. One hour is plenty
/// of headroom for `/simplify` + `/bump` on large repos and well shorter
/// than the time a wedged orchestrator would otherwise hold the project
/// hostage.
pub const STALE_TTL_SECS: i64 = 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MergePhase {
    /// `cc-hub pr merge` is executing the git operations (default).
    #[default]
    Merging,
    /// /simplify is running on main.
    Simplify,
    /// /bump is running on main.
    Bump,
    /// Post-/simplify, post-/bump; `pr finalize` not yet called.
    FinalizePending,
}

impl MergePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            MergePhase::Merging => "merging",
            MergePhase::Simplify => "simplify",
            MergePhase::Bump => "bump",
            MergePhase::FinalizePending => "finalize_pending",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "merging" => Some(MergePhase::Merging),
            "simplify" => Some(MergePhase::Simplify),
            "bump" => Some(MergePhase::Bump),
            "finalize_pending" | "finalize-pending" => Some(MergePhase::FinalizePending),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeLock {
    pub task_id: String,
    pub acquired_at: i64,
    /// tmux session of the orchestrator that holds the lock — used for
    /// liveness checks during stale detection.
    pub orchestrator_tmux: Option<String>,
    #[serde(default)]
    pub phase: MergePhase,
    /// The branch/ref the project root was on before `pr merge` checked out
    /// `base`. Stashed here so `pr finalize` can restore HEAD *after* the whole
    /// on-main Merging phase (build gate + /simplify + /bump) instead of
    /// `pr merge` bouncing HEAD back to the feature branch — which would run
    /// those on-main steps on the wrong branch. `None` when HEAD was detached
    /// or undeterminable, or before `pr merge` records it.
    #[serde(default)]
    pub prior_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcquireOutcome {
    Acquired,
    /// Another task currently holds the lock. The caller decides whether to
    /// poll again or surface the wait to the user.
    Held(MergeLock),
}

pub fn merge_lock_path(project_id: &str) -> Option<PathBuf> {
    orchestrator::project_state_dir(project_id).map(|d| d.join("merge.lock"))
}

/// Try to acquire the project's merge lock for `task_id`. Returns
/// [`AcquireOutcome::Acquired`] on success, [`AcquireOutcome::Held`] if
/// another live task already holds it. A pre-existing lock whose holder is
/// dead (no tmux session, or older than [`STALE_TTL_SECS`]) is overwritten.
///
/// Idempotent for the same `task_id`: if `task_id` already holds the lock
/// the call refreshes `acquired_at` and returns `Acquired`. This lets the
/// orchestrator retry without surprise after a transient failure.
pub fn acquire(
    project_id: &str,
    task_id: &str,
    orchestrator_tmux: Option<&str>,
) -> io::Result<AcquireOutcome> {
    let path = merge_lock_path(project_id).ok_or_else(|| io::Error::other("no home dir"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    with_guard(&path, || {
        let existing = read_lock_guarded(&path)?;

        if let Some(existing) = existing {
            if existing.task_id == task_id {
                // We already hold it — refresh `acquired_at`, preserving phase
                // and prior_ref so a re-acquire mid-/simplify/-bump doesn't
                // reset them.
                let refreshed = MergeLock {
                    task_id: task_id.to_string(),
                    acquired_at: orchestrator::now_unix_secs(),
                    orchestrator_tmux: orchestrator_tmux.map(str::to_string),
                    phase: existing.phase,
                    prior_ref: existing.prior_ref,
                };
                write_lock(&path, &refreshed)?;
                return Ok(AcquireOutcome::Acquired);
            }

            if !is_stale(&existing) {
                return Ok(AcquireOutcome::Held(existing));
            }

            log::info!(
                "merge_lock: clearing stale lock held by {} (age {}s, tmux alive: {})",
                existing.task_id,
                orchestrator::now_unix_secs() - existing.acquired_at,
                existing
                    .orchestrator_tmux
                    .as_deref()
                    .map(crate::send::tmux_session_exists)
                    .unwrap_or(false),
            );
            // Fall through: stale — the guard makes overwriting race-free.
        }

        let lock = MergeLock {
            task_id: task_id.to_string(),
            acquired_at: orchestrator::now_unix_secs(),
            orchestrator_tmux: orchestrator_tmux.map(str::to_string),
            phase: MergePhase::Merging,
            prior_ref: None,
        };
        write_lock(&path, &lock)?;
        Ok(AcquireOutcome::Acquired)
    })
}

/// Update the lock's phase if `task_id` is the current holder. Returns
/// `Ok(true)` on success, `Ok(false)` if there's no lock or the holder is
/// someone else.
pub fn set_phase(project_id: &str, task_id: &str, phase: MergePhase) -> io::Result<bool> {
    let path = merge_lock_path(project_id).ok_or_else(|| io::Error::other("no home dir"))?;
    with_guard(&path, || {
        let Some(mut existing) = read_lock_guarded(&path)? else {
            return Ok(false);
        };
        if existing.task_id != task_id {
            log::warn!(
                "merge_lock: task {} tried to set phase on lock held by {}",
                task_id,
                existing.task_id,
            );
            return Ok(false);
        }
        existing.phase = phase;
        write_lock(&path, &existing)?;
        Ok(true)
    })
}

/// Record the ref the project root was on before `pr merge` checked out the
/// base branch, so `pr finalize` can restore it after the on-main Merging
/// phase. Same holder-guard + atomic-write semantics as [`set_phase`]: returns
/// `Ok(false)` if there's no lock or the caller isn't the holder.
pub fn set_prior_ref(
    project_id: &str,
    task_id: &str,
    prior_ref: Option<String>,
) -> io::Result<bool> {
    let path = merge_lock_path(project_id).ok_or_else(|| io::Error::other("no home dir"))?;
    with_guard(&path, || {
        let Some(mut existing) = read_lock_guarded(&path)? else {
            return Ok(false);
        };
        if existing.task_id != task_id {
            log::warn!(
                "merge_lock: task {} tried to set prior_ref on lock held by {}",
                task_id,
                existing.task_id,
            );
            return Ok(false);
        }
        existing.prior_ref = prior_ref;
        write_lock(&path, &existing)?;
        Ok(true)
    })
}

/// Blocking variant of [`acquire`] for orchestrators that want to wait
/// rather than poll from outside. Loops in-process, retrying [`acquire`]
/// at `poll` cadence until it returns [`AcquireOutcome::Acquired`] or
/// `timeout` elapses; on timeout, returns the latest [`AcquireOutcome::Held`]
/// so the caller can surface the holder.
///
/// Re-uses [`acquire`] (rather than a bare `fs::metadata` watch) so each
/// poll inherits stale-lock recovery — a wait against a dead holder will
/// steal the lock on the first iteration instead of waiting out the
/// timeout.
pub fn acquire_blocking(
    project_id: &str,
    task_id: &str,
    orchestrator_tmux: Option<&str>,
    timeout: std::time::Duration,
    poll: std::time::Duration,
) -> io::Result<AcquireOutcome> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let outcome = acquire(project_id, task_id, orchestrator_tmux)?;
        if matches!(outcome, AcquireOutcome::Acquired) {
            return Ok(outcome);
        }
        if std::time::Instant::now() >= deadline {
            return Ok(outcome);
        }
        std::thread::sleep(poll);
    }
}

/// Release the lock if `task_id` is the current holder. Returns `Ok(false)`
/// if the lock didn't exist or is held by someone else (idempotent — a
/// double-release is not an error). Returns `Ok(true)` on actual release.
pub fn release(project_id: &str, task_id: &str) -> io::Result<bool> {
    let path = merge_lock_path(project_id).ok_or_else(|| io::Error::other("no home dir"))?;
    with_guard(&path, || {
        let Some(existing) = read_lock_guarded(&path)? else {
            return Ok(false);
        };
        if existing.task_id != task_id {
            log::warn!(
                "merge_lock: task {} tried to release lock held by {}",
                task_id,
                existing.task_id
            );
            return Ok(false);
        }
        // Holder verified under the guard — no steal can interleave before
        // the delete.
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    })
}

/// Read the current holder, if any. Returns `Ok(None)` if no lock exists.
/// Stale locks are returned as-is — the caller decides whether to honour
/// or steal.
pub fn current_holder(project_id: &str) -> io::Result<Option<MergeLock>> {
    let path = merge_lock_path(project_id).ok_or_else(|| io::Error::other("no home dir"))?;
    read_lock(&path)
}

fn is_stale(lock: &MergeLock) -> bool {
    let age = orchestrator::now_unix_secs() - lock.acquired_at;
    if age >= STALE_TTL_SECS {
        return true;
    }
    match lock.orchestrator_tmux.as_deref() {
        Some(tmux) => !crate::send::tmux_session_exists(tmux),
        // No tmux recorded — fall back to age-only.
        None => false,
    }
}

fn read_lock(path: &std::path::Path) -> io::Result<Option<MergeLock>> {
    match fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<MergeLock>(&raw) {
            Ok(lock) => Ok(Some(lock)),
            Err(e) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("merge.lock parse: {}", e),
            )),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Serialize a merge.lock mutation across processes: exclusive advisory flock
/// on the sidecar `merge.guard`, held for the duration of `f`. flock follows
/// the inode, so the guard must be a stable sidecar — the record itself is
/// tempfile+rename'd and can't be locked directly. The advisory lock
/// auto-releases on process death, so the guard can't strand the project the
/// way a stale record could.
fn with_guard<T>(path: &std::path::Path, f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    use fs2::FileExt;
    // Callers can run before the project state dir exists (e.g. a release or
    // prior_ref no-op against a never-locked project) — the guard file's
    // parent must exist to take the flock at all.
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let guard = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("guard"))?;
    guard.lock_exclusive()?;
    let result = f();
    let _ = guard.unlock();
    result
}

/// [`read_lock`] for use inside a guarded mutation: a corrupt record is
/// logged and treated as absent so the guarded writer can recover the
/// project by overwriting it, instead of every future acquire hard-failing.
/// Unguarded readers ([`current_holder`]) keep the strict error.
fn read_lock_guarded(path: &std::path::Path) -> io::Result<Option<MergeLock>> {
    match read_lock(path) {
        Err(e) if e.kind() == io::ErrorKind::InvalidData => {
            log::warn!("merge_lock: discarding corrupt lock record: {}", e);
            Ok(None)
        }
        other => other,
    }
}

fn write_lock(path: &std::path::Path, lock: &MergeLock) -> io::Result<()> {
    let body = serde_json::to_string_pretty(lock)
        .map_err(|e| io::Error::other(format!("serialise merge.lock: {}", e)))?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        use std::io::Write;
        let mut f = fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::HOME_TEST_LOCK;

    fn with_tempdir<F: FnOnce()>(f: F) {
        let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        f();
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn acquire_when_unlocked_succeeds() {
        with_tempdir(|| {
            match acquire("p1", "t-1", None).expect("acquire") {
                AcquireOutcome::Acquired => {}
                other => panic!("expected Acquired, got {:?}", other),
            }
            let lock = current_holder("p1").expect("read").expect("present");
            assert_eq!(lock.task_id, "t-1");
        });
    }

    #[test]
    fn acquire_when_held_by_other_returns_held() {
        with_tempdir(|| {
            let _ = acquire("p1", "t-1", None).expect("first acquire");
            match acquire("p1", "t-2", None).expect("second acquire") {
                AcquireOutcome::Held(l) => assert_eq!(l.task_id, "t-1"),
                other => panic!("expected Held, got {:?}", other),
            }
        });
    }

    #[test]
    fn acquire_is_idempotent_for_same_task() {
        with_tempdir(|| {
            let _ = acquire("p1", "t-1", None).expect("first");
            match acquire("p1", "t-1", None).expect("re-acquire") {
                AcquireOutcome::Acquired => {}
                other => panic!("expected re-acquired, got {:?}", other),
            }
        });
    }

    #[test]
    fn release_only_succeeds_for_holder() {
        with_tempdir(|| {
            let _ = acquire("p1", "t-1", None).expect("acquire");
            assert!(!release("p1", "t-2").expect("release wrong"));
            assert!(release("p1", "t-1").expect("release right"));
            assert!(current_holder("p1").expect("read").is_none());
        });
    }

    #[test]
    fn stale_lock_can_be_overwritten() {
        with_tempdir(|| {
            // Hand-write an aged lock with no tmux name.
            let path = merge_lock_path("p1").expect("path");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let stale = MergeLock {
                task_id: "t-old".into(),
                acquired_at: orchestrator::now_unix_secs() - STALE_TTL_SECS - 10,
                orchestrator_tmux: None,
                phase: MergePhase::Merging,
                prior_ref: None,
            };
            write_lock(&path, &stale).unwrap();

            match acquire("p1", "t-new", None).expect("acquire over stale") {
                AcquireOutcome::Acquired => {}
                other => panic!("expected to steal stale lock, got {:?}", other),
            }
            let lock = current_holder("p1").expect("read").expect("present");
            assert_eq!(lock.task_id, "t-new");
        });
    }

    #[test]
    fn release_when_no_lock_is_noop() {
        with_tempdir(|| {
            assert!(!release("p1", "t-1").expect("release noop"));
        });
    }

    #[test]
    fn lock_round_trips_without_phase_field() {
        with_tempdir(|| {
            let path = merge_lock_path("p1").expect("path");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            // Legacy on-disk shape, written before MergePhase existed.
            let legacy = r#"{
                "task_id": "t-legacy",
                "acquired_at": 1,
                "orchestrator_tmux": null
            }"#;
            fs::write(&path, legacy).unwrap();

            let lock = current_holder("p1")
                .expect("read")
                .expect("legacy lock present");
            assert_eq!(lock.task_id, "t-legacy");
            assert_eq!(lock.phase, MergePhase::Merging);
        });
    }

    #[test]
    fn set_phase_succeeds_for_holder() {
        with_tempdir(|| {
            let _ = acquire("p1", "t-1", None).expect("acquire");
            for phase in [
                MergePhase::Merging,
                MergePhase::Simplify,
                MergePhase::Bump,
                MergePhase::FinalizePending,
            ] {
                assert!(set_phase("p1", "t-1", phase).expect("set_phase"));
                let lock = current_holder("p1").expect("read").expect("present");
                assert_eq!(lock.phase, phase);
            }
        });
    }

    #[test]
    fn set_phase_rejects_non_holder() {
        with_tempdir(|| {
            let _ = acquire("p1", "t-1", None).expect("acquire");
            let before = current_holder("p1").expect("read").expect("present");
            assert!(!set_phase("p1", "t-2", MergePhase::Simplify).expect("set_phase"));
            let after = current_holder("p1").expect("read").expect("present");
            assert_eq!(before, after);
        });
    }

    #[test]
    fn acquire_refresh_preserves_phase() {
        with_tempdir(|| {
            let _ = acquire("p1", "t-1", None).expect("acquire");
            assert!(set_phase("p1", "t-1", MergePhase::Simplify).expect("set_phase"));
            let _ = acquire("p1", "t-1", None).expect("refresh");
            let lock = current_holder("p1").expect("read").expect("present");
            assert_eq!(lock.phase, MergePhase::Simplify);
        });
    }

    #[test]
    fn fresh_acquire_has_no_prior_ref() {
        with_tempdir(|| {
            let _ = acquire("p1", "t-1", None).expect("acquire");
            let lock = current_holder("p1").expect("read").expect("present");
            assert_eq!(lock.prior_ref, None);
        });
    }

    #[test]
    fn set_prior_ref_persists_and_survives_refresh() {
        with_tempdir(|| {
            let _ = acquire("p1", "t-1", None).expect("acquire");
            assert!(set_prior_ref("p1", "t-1", Some("dev".into())).expect("set_prior_ref"));
            let lock = current_holder("p1").expect("read").expect("present");
            assert_eq!(lock.prior_ref.as_deref(), Some("dev"));

            // A same-task re-acquire must not clobber the stashed prior_ref.
            let _ = acquire("p1", "t-1", None).expect("refresh");
            let lock = current_holder("p1").expect("read").expect("present");
            assert_eq!(lock.prior_ref.as_deref(), Some("dev"));
        });
    }

    #[test]
    fn set_prior_ref_rejects_non_holder() {
        with_tempdir(|| {
            let _ = acquire("p1", "t-1", None).expect("acquire");
            assert!(!set_prior_ref("p1", "t-2", Some("dev".into())).expect("set_prior_ref"));
            let lock = current_holder("p1").expect("read").expect("present");
            assert_eq!(lock.prior_ref, None);
        });
    }

    #[test]
    fn set_prior_ref_without_lock_is_noop() {
        with_tempdir(|| {
            assert!(!set_prior_ref("p1", "t-1", Some("dev".into())).expect("set_prior_ref"));
        });
    }

    #[test]
    fn acquire_over_existing_is_atomic_create_then_held() {
        // Two sequential acquires by different tasks: the first creates the
        // record under the guard, the second reads it under the guard and
        // returns Held without touching it.
        with_tempdir(|| {
            match acquire("p1", "t-1", None).expect("first") {
                AcquireOutcome::Acquired => {}
                other => panic!("expected Acquired, got {:?}", other),
            }
            match acquire("p1", "t-2", None).expect("second") {
                AcquireOutcome::Held(l) => assert_eq!(l.task_id, "t-1"),
                other => panic!("expected Held, got {:?}", other),
            }
            // Holder unchanged — the losing acquire must not have overwritten it.
            let lock = current_holder("p1").expect("read").expect("present");
            assert_eq!(lock.task_id, "t-1");
        });
    }

    #[test]
    fn acquire_recovers_from_corrupt_lock_record() {
        // A truncated/garbage merge.lock (disk corruption; the guarded writers
        // themselves never leave one) must not brick the project: a guarded
        // acquire logs, discards it, and takes the lock.
        with_tempdir(|| {
            let path = merge_lock_path("p1").expect("path");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "{ not json").unwrap();

            match acquire("p1", "t-1", None).expect("acquire over corrupt") {
                AcquireOutcome::Acquired => {}
                other => panic!("expected Acquired, got {:?}", other),
            }
            let lock = current_holder("p1").expect("read").expect("present");
            assert_eq!(lock.task_id, "t-1");
        });
    }

    #[test]
    fn concurrent_acquires_yield_exactly_one_winner() {
        // 8 threads race a fresh acquire for distinct tasks; the guard must
        // hand the lock to exactly one (the rest see Held).
        with_tempdir(|| {
            let home = std::env::var("HOME").expect("HOME set by with_tempdir");
            let handles: Vec<_> = (0..8)
                .map(|i| {
                    let home = home.clone();
                    std::thread::spawn(move || {
                        std::env::set_var("HOME", &home);
                        acquire("p1", &format!("t-{}", i), None).expect("acquire")
                    })
                })
                .collect();
            let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            let winners = outcomes
                .iter()
                .filter(|o| matches!(o, AcquireOutcome::Acquired))
                .count();
            assert_eq!(winners, 1, "outcomes: {:?}", outcomes);
        });
    }

    #[test]
    fn acquire_blocking_waits_for_release() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        with_tempdir(|| {
            // t-1 holds the lock.
            let _ = acquire("p1", "t-1", None).expect("first acquire");

            // Release after ~100ms in a background thread. We snapshot HOME
            // before spawning since the thread won't see env mutations from
            // its parent reliably.
            let released = Arc::new(AtomicBool::new(false));
            let released_clone = released.clone();
            let home = std::env::var("HOME").expect("HOME set by with_tempdir");
            let handle = std::thread::spawn(move || {
                std::env::set_var("HOME", &home);
                std::thread::sleep(std::time::Duration::from_millis(100));
                let _ = release("p1", "t-1").expect("release");
                released_clone.store(true, Ordering::SeqCst);
            });

            let started = std::time::Instant::now();
            let outcome = acquire_blocking(
                "p1",
                "t-2",
                None,
                std::time::Duration::from_secs(5),
                std::time::Duration::from_millis(20),
            )
            .expect("acquire_blocking");
            let elapsed = started.elapsed();

            handle.join().expect("release thread");
            assert!(released.load(Ordering::SeqCst), "releaser ran");
            assert!(
                matches!(outcome, AcquireOutcome::Acquired),
                "got {:?}",
                outcome
            );
            assert!(
                elapsed < std::time::Duration::from_secs(1),
                "elapsed {:?}",
                elapsed
            );
            let lock = current_holder("p1").expect("read").expect("present");
            assert_eq!(lock.task_id, "t-2");
        });
    }
}
