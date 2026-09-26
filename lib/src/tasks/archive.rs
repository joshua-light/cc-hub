use std::fs;
use std::io;
use std::path::PathBuf;

use super::store::{lock_exclusive, TaskState};
use crate::platform::paths::cc_hub_home;

fn archive_path() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("tasks-archive-v2.json"))
}

/// Append removed tasks to `~/.cc-hub/tasks-archive-v2.json` (a bare JSON
/// array of unified [`TaskState`]s), so `x` and `c` are recoverable beyond
/// the in-session undo slot. The archive is a log, not a ledger: an undone
/// delete leaves its copy behind, and a corrupt file starts fresh — same
/// policy as the board.
pub(super) fn archive_tasks(items: &[TaskState]) -> io::Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let Some(path) = archive_path() else {
        return Ok(());
    };
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("task archive path has no parent"))?;
    fs::create_dir_all(parent)?;
    let _lock = lock_exclusive(&parent.join("tasks-archive.lock"))?;
    let mut archived: Vec<TaskState> = match fs::read_to_string(&path) {
        Ok(raw) => {
            serde_json::from_str(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    archived.extend(items.iter().cloned());
    crate::persist::save_json(&path, &archived)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::tasks::store::TaskStatus;
    use crate::tasks::PersonalBoard;
    use crate::test_util::with_temp_home;

    #[test]
    fn removals_land_in_archive_and_restore_reinserts() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            let a = b.add("deleted one").unwrap().unwrap();
            let d = b.add("done one").unwrap().unwrap();
            b.set_status(&d, TaskStatus::Done).unwrap();
            let removed = b.remove(&a).unwrap().unwrap();
            b.clear_done().unwrap();
            // Both removal paths append to the archive file.
            let raw = fs::read_to_string(archive_path().unwrap()).unwrap();
            let archived: Vec<TaskState> = serde_json::from_str(&raw).unwrap();
            assert_eq!(
                archived
                    .iter()
                    .map(|t| t.task_id.as_str())
                    .collect::<Vec<_>>(),
                vec![a.as_str(), d.as_str()]
            );
            // Undo restores the task (with its id); a second restore of the
            // same id is refused.
            assert!(b.restore(removed.clone()).unwrap());
            assert!(!b.restore(removed).unwrap());
            assert_eq!(PersonalBoard::load().get(&a).unwrap().prompt, "deleted one");
        });
    }
}
