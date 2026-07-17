use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use crate::persist::save_json;
use crate::platform::paths::cc_hub_home;

/// Tracks user acknowledgements that force a session to display as Idle.
///
/// An ack is stamped with the session's `last_activity` at press time. While the
/// live watermark still matches, the session is treated as Idle regardless of
/// its real state (WaitingForInput or Processing). Any new activity advances
/// the watermark and auto-clears the ack.
///
/// Acks persist to `~/.cc-hub/acks.json` (via [`Acks::load`]) so a Space-idled
/// card stays idle across hub restarts and reboots — `last_activity` is derived
/// from transcript timestamps, so the stamped watermark stays comparable after
/// a restart. The file mirrors every mutation; there is no separate flush step.
#[derive(Default)]
pub struct Acks {
    entries: HashMap<String, Option<u64>>,
    /// When set, mutations are mirrored to this file. `None` (the [`Acks::new`]
    /// default) keeps the tracker purely in-memory — tests and hot-reload
    /// paths that must never touch the real home.
    path: Option<PathBuf>,
}

#[derive(Default, Serialize, Deserialize)]
struct OnDisk {
    #[serde(default)]
    acks: HashMap<String, Option<u64>>,
}

impl Acks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load persisted acks from `~/.cc-hub/acks.json`; subsequent mutations
    /// save back to the same file. A missing or unreadable file starts empty.
    pub fn load() -> Self {
        let path = acks_path();
        let entries = path
            .as_deref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|raw| serde_json::from_str::<OnDisk>(&raw).ok())
            .unwrap_or_default()
            .acks;
        Self { entries, path }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn ack(&mut self, session_id: &str, watermark: Option<u64>) {
        let prev = self.entries.insert(session_id.to_string(), watermark);
        if prev != Some(watermark) {
            self.save();
        }
    }

    /// Returns true if the ack still applies, false otherwise.
    /// Automatically removes stale entries whose watermark no longer matches.
    pub fn is_acked(&mut self, session_id: &str, current: Option<u64>) -> bool {
        match self.entries.get(session_id) {
            Some(stamped) if *stamped == current => true,
            Some(_) => {
                self.entries.remove(session_id);
                self.save();
                false
            }
            None => false,
        }
    }

    /// Drop acks for session ids that no longer appear.
    pub fn retain_existing(&mut self, live_ids: &HashSet<&str>) {
        let before = self.entries.len();
        self.entries.retain(|id, _| live_ids.contains(id.as_str()));
        if self.entries.len() != before {
            self.save();
        }
    }

    /// Persistence errors are swallowed after logging: acks are a display
    /// convenience, and the caller has no useful recovery.
    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let on_disk = OnDisk {
            acks: self.entries.clone(),
        };
        if let Err(e) = save_json(path, &on_disk) {
            log::warn!("acks: failed to persist {}: {}", path.display(), e);
        }
    }
}

fn acks_path() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("acks.json"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_util::with_temp_home;

    #[test]
    fn ack_persists_across_reload() {
        with_temp_home(|| {
            let mut acks = Acks::load();
            acks.ack("s-1", Some(100));
            acks.ack("s-2", None);

            let mut reloaded = Acks::load();
            assert!(reloaded.is_acked("s-1", Some(100)));
            assert!(reloaded.is_acked("s-2", None));
        });
    }

    #[test]
    fn new_activity_clears_ack_on_disk() {
        with_temp_home(|| {
            let mut acks = Acks::load();
            acks.ack("s-1", Some(100));

            // A reloaded instance sees the watermark advance, drops the entry,
            // and persists the removal — the old stamp must not resurrect.
            let mut reloaded = Acks::load();
            assert!(!reloaded.is_acked("s-1", Some(200)));

            let mut again = Acks::load();
            assert!(!again.is_acked("s-1", Some(100)));
        });
    }

    #[test]
    fn retain_existing_persists_removals() {
        with_temp_home(|| {
            let mut acks = Acks::load();
            acks.ack("gone", Some(1));
            acks.ack("kept", Some(2));

            let live: HashSet<&str> = ["kept"].into_iter().collect();
            acks.retain_existing(&live);

            let mut reloaded = Acks::load();
            assert!(reloaded.is_acked("kept", Some(2)));
            assert!(!reloaded.is_acked("gone", Some(1)));
        });
    }

    #[test]
    fn in_memory_new_never_writes() {
        with_temp_home(|| {
            let mut acks = Acks::new();
            acks.ack("s-1", Some(100));

            let mut reloaded = Acks::load();
            assert!(!reloaded.is_acked("s-1", Some(100)));
        });
    }

    #[test]
    fn missing_file_yields_empty() {
        with_temp_home(|| {
            let acks = Acks::load();
            assert!(acks.is_empty());
        });
    }
}
