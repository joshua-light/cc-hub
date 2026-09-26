use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;

use crate::platform::paths::cc_hub_home;

/// Board-level metadata that isn't per-task: currently just the cwd of the
/// most recent assignment, kept across restarts so the assign picker promotes
/// it to the top of the places list — firing several tasks at one project is
/// a plain Enter each time.
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub(super) struct BoardMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) last_assign_cwd: Option<String>,
}

fn board_meta_path() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("board.json"))
}

pub(super) fn load_board_meta() -> BoardMeta {
    let Some(path) = board_meta_path() else {
        return BoardMeta::default();
    };
    fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub(super) fn save_board_meta(meta: &BoardMeta) -> io::Result<()> {
    let Some(path) = board_meta_path() else {
        return Ok(());
    };
    crate::persist::save_json(&path, meta)
}

#[cfg(all(test, unix))]
mod tests {
    use crate::tasks::PersonalBoard;
    use crate::test_util::with_temp_home;

    #[test]
    fn assign_records_last_assign_cwd_across_reloads() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            assert_eq!(b.last_assign_cwd(), None);
            let id = b.add("t").unwrap().unwrap();
            b.assign(&id, "/tmp/proj", "claude", "cchub-1-42").unwrap();
            assert_eq!(PersonalBoard::load().last_assign_cwd(), Some("/tmp/proj"));
            // Deleting the task must not forget where it ran.
            b.remove(&id).unwrap();
            assert_eq!(PersonalBoard::load().last_assign_cwd(), Some("/tmp/proj"));
        });
    }
}
