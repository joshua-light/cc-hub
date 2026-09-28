//! Space-to-hold: sessions parked in the On hold section, persisted to
//! `~/.cc-hub/holds.json`; see [`Holds`].

use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::PathBuf;

use crate::persist::save_json;
use crate::platform::paths::cc_hub_home;

/// Sessions the user put on hold. A held session keeps its real state; it
/// only leaves its project group for the On hold section at the bottom of
/// the grid, and stops counting toward attention.
///
/// Unlike an [`crate::acks::Acks`] entry, a hold ignores activity: it lasts
/// until Space releases it or the session drops out of the scan. It
/// persists to `~/.cc-hub/holds.json` (via [`Holds::load`]), mirrored on
/// every mutation.
#[derive(Default)]
pub struct Holds {
    ids: BTreeSet<String>,
    /// When set, mutations are mirrored to this file. `None` (the
    /// [`Holds::new`] default) keeps the set in memory, for tests that must
    /// never touch the real home.
    path: Option<PathBuf>,
}

#[derive(Default, Serialize, Deserialize)]
struct OnDisk {
    #[serde(default)]
    holds: BTreeSet<String>,
}

impl Holds {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load persisted holds from `~/.cc-hub/holds.json`; subsequent
    /// mutations save back to the same file. A missing or unreadable file
    /// starts empty.
    pub fn load() -> Self {
        let path = holds_path();
        let ids = path
            .as_deref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|raw| serde_json::from_str::<OnDisk>(&raw).ok())
            .unwrap_or_default()
            .holds;
        Self { ids, path }
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn contains(&self, session_id: &str) -> bool {
        self.ids.contains(session_id)
    }

    /// Put the session on hold, or release it. Returns whether it is held
    /// now.
    pub fn toggle(&mut self, session_id: &str) -> bool {
        let held = !self.ids.remove(session_id);
        if held {
            self.ids.insert(session_id.to_string());
        }
        self.save();
        held
    }

    /// Drop holds for session ids that no longer appear.
    pub fn retain_existing(&mut self, live_ids: &HashSet<&str>) {
        let before = self.ids.len();
        self.ids.retain(|id| live_ids.contains(id.as_str()));
        if self.ids.len() != before {
            self.save();
        }
    }

    /// Persistence errors are swallowed after logging, as for acks: the
    /// caller has no useful recovery.
    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let on_disk = OnDisk {
            holds: self.ids.clone(),
        };
        if let Err(e) = save_json(path, &on_disk) {
            log::warn!("holds: failed to persist {}: {}", path.display(), e);
        }
    }
}

fn holds_path() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("holds.json"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_util::with_temp_home;

    #[test]
    fn toggle_holds_then_releases() {
        let mut holds = Holds::new();
        assert!(holds.toggle("s-1"));
        assert!(holds.contains("s-1"));
        assert!(!holds.toggle("s-1"));
        assert!(!holds.contains("s-1"));
    }

    #[test]
    fn hold_persists_across_reload() {
        with_temp_home(|| {
            let mut holds = Holds::load();
            holds.toggle("s-1");
            holds.toggle("s-2");
            holds.toggle("s-2");

            let reloaded = Holds::load();
            assert!(reloaded.contains("s-1"));
            assert!(!reloaded.contains("s-2"), "a release persists too");
        });
    }

    #[test]
    fn retain_existing_persists_removals() {
        with_temp_home(|| {
            let mut holds = Holds::load();
            holds.toggle("gone");
            holds.toggle("kept");

            let live: HashSet<&str> = ["kept"].into_iter().collect();
            holds.retain_existing(&live);

            let reloaded = Holds::load();
            assert!(reloaded.contains("kept"));
            assert!(!reloaded.contains("gone"));
        });
    }

    #[test]
    fn in_memory_new_never_writes() {
        with_temp_home(|| {
            let mut holds = Holds::new();
            holds.toggle("s-1");
            assert!(Holds::load().is_empty());
        });
    }
}
