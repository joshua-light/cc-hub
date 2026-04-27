//! Cross-orchestrator file coordination via two-phase soft reservations.
//!
//! Per-project state: `~/.cc-hub/projects/<project_id>/reservations.json`,
//! protected by an advisory `flock` on the sidecar `reservations.lock`. The
//! lock is held only for the read-modify-write critical section; readers also
//! take it (shared with the same exclusive call site) so we never tear a
//! reader against a concurrent writer's tempfile rename.
//!
//! Two phases:
//! - `intended` — orchestrator declared its plan; no worker yet edits.
//! - `active`   — a worktree worker is actively editing these files.
//!
//! Stale reservations (no heartbeat in `RESERVATION_TTL_SECS`) are filtered
//! by readers and removed lazily on the next write — recovery from crashed
//! orchestrators without an election protocol.
//!
//! Overlap rule for path matching: paths overlap if equal, OR either side
//! ends with `/` and the other starts with it (directory prefix). No globs.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::orchestrator::{cc_hub_home, now_unix_secs};

pub const RESERVATION_TTL_SECS: i64 = 600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Intended,
    Active,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub task_id: String,
    #[serde(default)]
    pub worker_id: Option<String>,
    pub phase: Phase,
    pub paths: Vec<String>,
    pub owner_session: String,
    pub created_at: i64,
    pub last_heartbeat: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReservationsFile {
    pub version: u32,
    #[serde(default)]
    pub reservations: Vec<Reservation>,
}

impl Default for ReservationsFile {
    fn default() -> Self {
        Self {
            version: 1,
            reservations: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeOutcome {
    Ok(Reservation),
    Conflict {
        task_id: String,
        paths: Vec<String>,
    },
}

// ─── public path helpers ─────────────────────────────────────────────────

pub fn reservations_path(project_id: &str) -> Option<PathBuf> {
    cc_hub_home().map(|h| reservations_path_in(&h, project_id))
}

pub fn reservations_lock_path(project_id: &str) -> Option<PathBuf> {
    cc_hub_home().map(|h| reservations_lock_path_in(&h, project_id))
}

fn reservations_path_in(home: &Path, project_id: &str) -> PathBuf {
    home.join("projects").join(project_id).join("reservations.json")
}

fn reservations_lock_path_in(home: &Path, project_id: &str) -> PathBuf {
    home.join("projects").join(project_id).join("reservations.lock")
}

fn home_or_err() -> io::Result<PathBuf> {
    cc_hub_home().ok_or_else(|| io::Error::other("no home dir"))
}

// ─── public API ──────────────────────────────────────────────────────────

pub fn read_reservations(project_id: &str) -> io::Result<ReservationsFile> {
    let home = home_or_err()?;
    read_reservations_in(&home, project_id)
}

pub fn declare(
    project_id: &str,
    task_id: &str,
    paths: &[String],
    owner_session: &str,
    replace: bool,
) -> io::Result<Reservation> {
    let home = home_or_err()?;
    declare_in(&home, project_id, task_id, paths, owner_session, replace)
}

pub fn upgrade_to_active(
    project_id: &str,
    task_id: &str,
    worker_id: &str,
    paths: &[String],
) -> io::Result<UpgradeOutcome> {
    let home = home_or_err()?;
    upgrade_to_active_in(&home, project_id, task_id, worker_id, paths)
}

pub fn downgrade(
    project_id: &str,
    task_id: &str,
    worker_id: Option<&str>,
) -> io::Result<()> {
    let home = home_or_err()?;
    downgrade_in(&home, project_id, task_id, worker_id)
}

pub fn release(
    project_id: &str,
    task_id: &str,
    worker_id: Option<&str>,
) -> io::Result<()> {
    let home = home_or_err()?;
    release_in(&home, project_id, task_id, worker_id)
}

pub fn refresh_heartbeat(project_id: &str, task_id: &str) -> io::Result<()> {
    let home = home_or_err()?;
    refresh_heartbeat_in(&home, project_id, task_id)
}

pub fn overlapping_active(
    project_id: &str,
    task_id_excluded: &str,
    paths: &[String],
) -> io::Result<Vec<(String, Vec<String>)>> {
    let home = home_or_err()?;
    overlapping_active_in(&home, project_id, task_id_excluded, paths)
}

pub fn list(project_id: &str, include_stale: bool) -> io::Result<Vec<Reservation>> {
    let home = home_or_err()?;
    list_in(&home, project_id, include_stale)
}

// ─── _in variants (test-friendly: explicit cc-hub home) ──────────────────

pub fn read_reservations_in(home: &Path, project_id: &str) -> io::Result<ReservationsFile> {
    let path = reservations_path_in(home, project_id);
    match fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {}", path.display(), e),
            )
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(ReservationsFile::default()),
        Err(e) => Err(e),
    }
}

pub fn declare_in(
    home: &Path,
    project_id: &str,
    task_id: &str,
    paths: &[String],
    owner_session: &str,
    replace: bool,
) -> io::Result<Reservation> {
    with_lock(home, project_id, |file| {
        let now = now_unix_secs();
        let normalized: Vec<String> = paths.iter().filter(|p| !p.is_empty()).cloned().collect();
        // Find an existing intended entry for this task (worker_id is None
        // for the per-task umbrella entry). If present, merge or replace.
        let pos = file.reservations.iter().position(|r| {
            r.task_id == task_id && r.worker_id.is_none() && r.phase == Phase::Intended
        });
        let entry = match pos {
            Some(idx) => {
                let r = &mut file.reservations[idx];
                if replace {
                    r.paths = normalized;
                } else {
                    for p in normalized {
                        if !r.paths.contains(&p) {
                            r.paths.push(p);
                        }
                    }
                }
                r.owner_session = owner_session.to_string();
                r.last_heartbeat = now;
                r.clone()
            }
            None => {
                let r = Reservation {
                    task_id: task_id.to_string(),
                    worker_id: None,
                    phase: Phase::Intended,
                    paths: normalized,
                    owner_session: owner_session.to_string(),
                    created_at: now,
                    last_heartbeat: now,
                };
                file.reservations.push(r.clone());
                r
            }
        };
        Ok(entry)
    })
}

pub fn upgrade_to_active_in(
    home: &Path,
    project_id: &str,
    task_id: &str,
    worker_id: &str,
    paths: &[String],
) -> io::Result<UpgradeOutcome> {
    with_lock(home, project_id, |file| {
        let now = now_unix_secs();
        // Stale-aware overlap check against OTHER tasks' active reservations.
        for r in file.reservations.iter() {
            if r.task_id == task_id {
                continue;
            }
            if r.phase != Phase::Active {
                continue;
            }
            if is_stale(r, now) {
                continue;
            }
            let overlap: Vec<String> = paths
                .iter()
                .filter(|q| r.paths.iter().any(|rp| paths_overlap(rp, q)))
                .cloned()
                .collect();
            if !overlap.is_empty() {
                return Ok(UpgradeOutcome::Conflict {
                    task_id: r.task_id.clone(),
                    paths: r.paths.clone(),
                });
            }
        }

        let owner_session = file
            .reservations
            .iter()
            .find(|r| r.task_id == task_id)
            .map(|r| r.owner_session.clone())
            .unwrap_or_default();

        let normalized: Vec<String> = paths.iter().filter(|p| !p.is_empty()).cloned().collect();

        // Replace any existing entry for this (task_id, worker_id) — a worker
        // calling upgrade twice should refresh, not duplicate.
        let pos = file.reservations.iter().position(|r| {
            r.task_id == task_id && r.worker_id.as_deref() == Some(worker_id)
        });
        let entry = match pos {
            Some(idx) => {
                let r = &mut file.reservations[idx];
                r.phase = Phase::Active;
                r.paths = normalized;
                r.last_heartbeat = now;
                r.clone()
            }
            None => {
                let r = Reservation {
                    task_id: task_id.to_string(),
                    worker_id: Some(worker_id.to_string()),
                    phase: Phase::Active,
                    paths: normalized,
                    owner_session,
                    created_at: now,
                    last_heartbeat: now,
                };
                file.reservations.push(r.clone());
                r
            }
        };
        Ok(UpgradeOutcome::Ok(entry))
    })
}

pub fn downgrade_in(
    home: &Path,
    project_id: &str,
    task_id: &str,
    worker_id: Option<&str>,
) -> io::Result<()> {
    with_lock(home, project_id, |file| {
        let now = now_unix_secs();
        for r in file.reservations.iter_mut() {
            if r.task_id != task_id {
                continue;
            }
            if r.phase != Phase::Active {
                continue;
            }
            let matches_worker = match worker_id {
                Some(w) => r.worker_id.as_deref() == Some(w),
                None => true,
            };
            if matches_worker {
                r.phase = Phase::Intended;
                r.worker_id = None;
                r.last_heartbeat = now;
            }
        }
        Ok(())
    })
}

pub fn release_in(
    home: &Path,
    project_id: &str,
    task_id: &str,
    worker_id: Option<&str>,
) -> io::Result<()> {
    with_lock(home, project_id, |file| {
        file.reservations.retain(|r| {
            if r.task_id != task_id {
                return true;
            }
            match worker_id {
                Some(w) => r.worker_id.as_deref() != Some(w),
                None => false,
            }
        });
        Ok(())
    })
}

pub fn refresh_heartbeat_in(home: &Path, project_id: &str, task_id: &str) -> io::Result<()> {
    // Cheap pre-check: most `task report` calls come from tasks with no
    // active reservations (read-only workers, early in life). Skip the lock
    // and rename in that case.
    let pre = read_reservations_in(home, project_id).unwrap_or_default();
    if !pre.reservations.iter().any(|r| r.task_id == task_id) {
        return Ok(());
    }
    with_lock(home, project_id, |file| {
        let now = now_unix_secs();
        for r in file.reservations.iter_mut() {
            if r.task_id == task_id {
                r.last_heartbeat = now;
            }
        }
        Ok(())
    })
}

pub fn overlapping_active_in(
    home: &Path,
    project_id: &str,
    task_id_excluded: &str,
    paths: &[String],
) -> io::Result<Vec<(String, Vec<String>)>> {
    let file = read_reservations_in(home, project_id)?;
    let now = now_unix_secs();
    let mut grouped: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for r in file.reservations.iter() {
        if r.task_id == task_id_excluded {
            continue;
        }
        if r.phase != Phase::Active {
            continue;
        }
        if is_stale(r, now) {
            continue;
        }
        let mut hits: Vec<String> = Vec::new();
        for q in paths.iter() {
            for rp in r.paths.iter() {
                if paths_overlap(rp, q) && !hits.contains(rp) {
                    hits.push(rp.clone());
                }
            }
        }
        if !hits.is_empty() {
            grouped
                .entry(r.task_id.clone())
                .and_modify(|v| {
                    for h in &hits {
                        if !v.contains(h) {
                            v.push(h.clone());
                        }
                    }
                })
                .or_insert(hits);
        }
    }
    Ok(grouped.into_iter().collect())
}

pub fn list_in(
    home: &Path,
    project_id: &str,
    include_stale: bool,
) -> io::Result<Vec<Reservation>> {
    let file = read_reservations_in(home, project_id)?;
    let now = now_unix_secs();
    Ok(file
        .reservations
        .into_iter()
        .filter(|r| include_stale || !is_stale(r, now))
        .collect())
}

// ─── overlap + stale primitives ──────────────────────────────────────────

pub fn paths_overlap(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if a.ends_with('/') && b.starts_with(a) {
        return true;
    }
    if b.ends_with('/') && a.starts_with(b) {
        return true;
    }
    false
}

fn is_stale(r: &Reservation, now: i64) -> bool {
    now.saturating_sub(r.last_heartbeat) > RESERVATION_TTL_SECS
}

// ─── lock + read-modify-write plumbing ───────────────────────────────────

fn with_lock<R>(
    home: &Path,
    project_id: &str,
    f: impl FnOnce(&mut ReservationsFile) -> io::Result<R>,
) -> io::Result<R> {
    let dir = home.join("projects").join(project_id);
    fs::create_dir_all(&dir)?;
    let lock_path = reservations_lock_path_in(home, project_id);
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    lock_file.lock_exclusive()?;

    // Read inside the critical section so writers we serialize after see
    // the post-rename state, not a stale snapshot.
    let mut file = read_reservations_in(home, project_id)?;
    // Lazily evict stale entries on every write; readers already filter them
    // but the file shouldn't grow forever.
    let now = now_unix_secs();
    file.reservations.retain(|r| !is_stale(r, now));

    let result = f(&mut file)?;
    file.version = 1;
    write_reservations_in(home, project_id, &file)?;
    // Lock auto-released when `lock_file` drops, but be explicit on success
    // so the unlock isn't tangled with destructor ordering on early returns.
    let _ = FileExt::unlock(&lock_file);
    Ok(result)
}

fn write_reservations_in(
    home: &Path,
    project_id: &str,
    file: &ReservationsFile,
) -> io::Result<()> {
    let path = reservations_path_in(home, project_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(file)
        .map_err(|e| io::Error::other(format!("serialize reservations: {}", e)))?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        use std::io::Write;
        let mut f = fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn paths_overlap_basics() {
        assert!(paths_overlap("a.rs", "a.rs"));
        assert!(!paths_overlap("a.rs", "b.rs"));
        assert!(paths_overlap("lib/", "lib/foo.rs"));
        assert!(paths_overlap("lib/foo.rs", "lib/"));
        assert!(!paths_overlap("lib/", "src/foo.rs"));
        // The "/" suffix is significant — without it the prefix relation
        // could falsely match `lib_other/` to `lib/foo.rs`.
        assert!(!paths_overlap("lib", "lib/foo.rs"));
    }

    #[test]
    fn is_stale_uses_ttl() {
        let r = Reservation {
            task_id: "t".into(),
            worker_id: None,
            phase: Phase::Intended,
            paths: vec![],
            owner_session: "".into(),
            created_at: 0,
            last_heartbeat: 0,
        };
        assert!(is_stale(&r, RESERVATION_TTL_SECS + 1));
        assert!(!is_stale(&r, RESERVATION_TTL_SECS));
    }
}
