//! Project-level merge lock — at most one task per project may be in the
//! `Merging` phase at any moment. The PR-flow design serializes merges on
//! purpose: each merging task fetches main, resolves conflicts against
//! whatever just landed, and then merges into a known-stable target. With
//! serialization the conflict-resolution step can never see the target
//! shifting under it.
//!
//! The lock lives at `~/.cc-hub/projects/<pid>/merge.lock` and contains a
//! JSON record naming the holder. Acquisition is `O_EXCL` create — the
//! filesystem decides who wins a race. Release is a simple delete.
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeLock {
    pub task_id: String,
    pub acquired_at: i64,
    /// tmux session of the orchestrator that holds the lock — used for
    /// liveness checks during stale detection.
    pub orchestrator_tmux: Option<String>,
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
    let path = merge_lock_path(project_id)
        .ok_or_else(|| io::Error::other("no home dir"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    if let Some(existing) = read_lock(&path)? {
        if existing.task_id == task_id {
            // We already hold it — refresh and return Acquired.
            let refreshed = MergeLock {
                task_id: task_id.to_string(),
                acquired_at: orchestrator::now_unix_secs(),
                orchestrator_tmux: orchestrator_tmux.map(str::to_string),
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
        // Fall through: stale, can be overwritten.
    }

    let lock = MergeLock {
        task_id: task_id.to_string(),
        acquired_at: orchestrator::now_unix_secs(),
        orchestrator_tmux: orchestrator_tmux.map(str::to_string),
    };
    write_lock(&path, &lock)?;
    Ok(AcquireOutcome::Acquired)
}

/// Release the lock if `task_id` is the current holder. Returns `Ok(false)`
/// if the lock didn't exist or is held by someone else (idempotent — a
/// double-release is not an error). Returns `Ok(true)` on actual release.
pub fn release(project_id: &str, task_id: &str) -> io::Result<bool> {
    let path = merge_lock_path(project_id)
        .ok_or_else(|| io::Error::other("no home dir"))?;
    let Some(existing) = read_lock(&path)? else {
        return Ok(false);
    };
    if existing.task_id != task_id {
        log::warn!(
            "merge_lock: task {} tried to release lock held by {}",
            task_id, existing.task_id
        );
        return Ok(false);
    }
    fs::remove_file(&path)?;
    Ok(true)
}

/// Read the current holder, if any. Returns `Ok(None)` if no lock exists.
/// Stale locks are returned as-is — the caller decides whether to honour
/// or steal.
pub fn current_holder(project_id: &str) -> io::Result<Option<MergeLock>> {
    let path = merge_lock_path(project_id)
        .ok_or_else(|| io::Error::other("no home dir"))?;
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
}
