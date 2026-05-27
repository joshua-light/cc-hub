//! Persistent list of folders the user has bookmarked in the session
//! picker. Stored at `~/.cc-hub/bookmarks.json` as a flat array of absolute
//! paths so the file is trivially editable by hand if needed.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::platform::paths::cc_hub_home;

#[derive(Default, Clone, Debug)]
pub struct Bookmarks {
    paths: BTreeSet<PathBuf>,
}

#[derive(Default, Serialize, Deserialize)]
struct OnDisk {
    #[serde(default)]
    folders: Vec<PathBuf>,
}

impl Bookmarks {
    pub fn load() -> Self {
        let Some(path) = bookmarks_path() else {
            return Self::default();
        };
        let Ok(raw) = fs::read_to_string(&path) else {
            return Self::default();
        };
        let parsed: OnDisk = serde_json::from_str(&raw).unwrap_or_default();
        Self {
            paths: parsed.folders.into_iter().collect(),
        }
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.paths.contains(path)
    }

    pub fn list(&self) -> Vec<PathBuf> {
        self.paths.iter().cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Add or remove `path`. Returns `true` if the path is now bookmarked,
    /// `false` if it was just removed. Persists to disk; persistence errors
    /// are swallowed (bookmarks are a convenience, not a correctness
    /// guarantee — the caller has no useful recovery beyond logging).
    pub fn toggle(&mut self, path: PathBuf) -> bool {
        let added = if self.paths.contains(&path) {
            self.paths.remove(&path);
            false
        } else {
            self.paths.insert(path);
            true
        };
        let _ = self.save();
        added
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = bookmarks_path() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let on_disk = OnDisk {
            folders: self.paths.iter().cloned().collect(),
        };
        let raw = serde_json::to_string_pretty(&on_disk).map_err(std::io::Error::other)?;
        fs::write(&path, raw)
    }
}

fn bookmarks_path() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("bookmarks.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::HOME_TEST_LOCK;

    fn with_temp_home<F: FnOnce()>(f: F) {
        let _guard = HOME_TEST_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());
        f();
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn toggle_persists_round_trip() {
        with_temp_home(|| {
            let mut b = Bookmarks::load();
            assert!(b.is_empty());
            assert!(b.toggle(PathBuf::from("/tmp/foo")));
            assert!(b.toggle(PathBuf::from("/tmp/bar")));
            let reloaded = Bookmarks::load();
            assert!(reloaded.contains(Path::new("/tmp/foo")));
            assert!(reloaded.contains(Path::new("/tmp/bar")));
        });
    }

    #[test]
    fn toggle_off_removes() {
        with_temp_home(|| {
            let mut b = Bookmarks::load();
            b.toggle(PathBuf::from("/tmp/foo"));
            assert!(!b.toggle(PathBuf::from("/tmp/foo")));
            let reloaded = Bookmarks::load();
            assert!(!reloaded.contains(Path::new("/tmp/foo")));
        });
    }

    #[test]
    fn missing_file_yields_empty() {
        with_temp_home(|| {
            let b = Bookmarks::load();
            assert!(b.is_empty());
        });
    }
}
