//! User-driven session→task links behind `L` on the Sessions tab.
//!
//! Sessions are discovered read-only from agent transcripts, so the link
//! lives in a sidecar keyed by session id — `~/.cc-hub/session-tasks.json` —
//! the same pattern as the rename sidecar (`session-titles.json`). The
//! session side owns the link (one task ↔ many sessions), so nothing here
//! touches the lock-serialized `TaskState` files.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::platform::paths::cc_hub_home;

/// One session's link to a task. `title` is a snapshot taken at link time
/// (refreshed while the task is readable) so the Sessions grid can still
/// label the group after the task is archived or deleted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLink {
    pub task_id: String,
    /// `None` for a personal-board task, `Some` for an orchestrated one —
    /// mirrors `TaskState::project_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Task title snapshot; the grid's fallback label once the task is gone.
    #[serde(default)]
    pub title: String,
}

#[derive(Default, Serialize, Deserialize)]
struct OnDisk {
    #[serde(default)]
    links: HashMap<String, TaskLink>,
}

/// Serializes concurrent writers so a load/insert/save cycle can't race
/// another's. Readers are independently safe thanks to the tmp+rename in
/// [`crate::persist::save_json`].
static WRITE_LOCK: Mutex<()> = Mutex::new(());

fn store_path() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("session-tasks.json"))
}

/// Current on-disk map of `session_id → TaskLink`. Empty on any read/parse
/// failure — a missing store is the normal first-run state.
pub fn load() -> HashMap<String, TaskLink> {
    let Some(path) = store_path() else {
        return HashMap::new();
    };
    let Ok(raw) = fs::read_to_string(&path) else {
        return HashMap::new();
    };
    match serde_json::from_str::<OnDisk>(&raw) {
        Ok(v) => v.links,
        Err(e) => {
            log::warn!("session-tasks parse error at {}: {}", path.display(), e);
            HashMap::new()
        }
    }
}

/// Atomically upsert `sid → link`. Holds [`WRITE_LOCK`] across the
/// load/insert/save cycle so two writers can't clobber each other's entries.
pub fn link(sid: &str, link: TaskLink) -> io::Result<()> {
    mutate(|links| {
        links.insert(sid.to_string(), link);
    })
}

/// Atomically drop `sid`'s link. Removing a link that doesn't exist is a
/// no-op, not an error.
pub fn unlink(sid: &str) -> io::Result<()> {
    mutate(|links| {
        links.remove(sid);
    })
}

fn mutate(f: impl FnOnce(&mut HashMap<String, TaskLink>)) -> io::Result<()> {
    let Some(path) = store_path() else {
        return Ok(());
    };
    let _g = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut links = load();
    f(&mut links);
    crate::persist::save_json(&path, &OnDisk { links })
}

// Unix-only: isolation works by redirecting `$HOME`, which `dirs::home_dir()`
// ignores on Windows — same policy as bookmarks.rs / tasks.rs.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_util::with_temp_home;

    fn sample(task_id: &str) -> TaskLink {
        TaskLink {
            task_id: task_id.into(),
            project_id: None,
            title: "Fix auth".into(),
        }
    }

    #[test]
    fn link_persists_round_trip() {
        with_temp_home(|| {
            assert!(load().is_empty());
            link("sid-1", sample("tk-1")).unwrap();
            link("sid-2", sample("tk-2")).unwrap();
            let map = load();
            assert_eq!(map.len(), 2);
            assert_eq!(map.get("sid-1").unwrap().task_id, "tk-1");
        });
    }

    #[test]
    fn relink_replaces_and_unlink_removes() {
        with_temp_home(|| {
            link("sid-1", sample("tk-1")).unwrap();
            link("sid-1", sample("tk-2")).unwrap();
            assert_eq!(load().get("sid-1").unwrap().task_id, "tk-2");
            unlink("sid-1").unwrap();
            assert!(load().is_empty());
            // Unlinking a missing sid is a quiet no-op.
            unlink("sid-1").unwrap();
        });
    }

    #[test]
    fn corrupt_store_reads_as_empty() {
        with_temp_home(|| {
            let path = store_path().unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "{not-json").unwrap();
            assert!(load().is_empty());
        });
    }
}
