//! Persistent scratch to-do list shown as a side panel on the Sessions tab
//! (toggled with `t`). It's a lightweight place to jot things to do while
//! driving agents — not tied to any session or project. Stored at
//! `~/.cc-hub/todo.json` as an ordered array of `{ text, done }` items so the
//! file is trivially editable by hand if needed.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::platform::paths::cc_hub_home;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub text: String,
    #[serde(default)]
    pub done: bool,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct TodoList {
    #[serde(default)]
    items: Vec<TodoItem>,
}

impl TodoList {
    /// Load the list from disk. A missing or unparseable file yields an empty
    /// list — the to-do list is a convenience, not a correctness guarantee, so
    /// we'd rather start fresh than refuse to open.
    pub fn load() -> Self {
        let Some(path) = todo_path() else {
            return Self::default();
        };
        let Ok(raw) = fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    pub fn items(&self) -> &[TodoItem] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Append a task (trimmed). Empty/whitespace-only input is ignored.
    /// Returns the index of the new item, or `None` if nothing was added.
    /// Persists immediately.
    pub fn add(&mut self, text: &str) -> Option<usize> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        self.items.push(TodoItem {
            text: text.to_string(),
            done: false,
        });
        let _ = self.save();
        Some(self.items.len() - 1)
    }

    /// Flip the done/undone flag of the item at `idx`. No-op if out of range.
    /// Persists immediately.
    pub fn toggle(&mut self, idx: usize) {
        if let Some(item) = self.items.get_mut(idx) {
            item.done = !item.done;
            let _ = self.save();
        }
    }

    /// Remove the item at `idx`. No-op if out of range. Persists immediately.
    pub fn remove(&mut self, idx: usize) {
        if idx < self.items.len() {
            self.items.remove(idx);
            let _ = self.save();
        }
    }

    /// Drop every completed item, preserving the order of the rest. Returns the
    /// number removed. Only persists when something actually changed, so a
    /// no-op stroke doesn't rewrite the file.
    pub fn clear_completed(&mut self) -> usize {
        let before = self.items.len();
        self.items.retain(|item| !item.done);
        let removed = before - self.items.len();
        if removed > 0 {
            let _ = self.save();
        }
        removed
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = todo_path() else {
            return Ok(());
        };
        crate::persist::save_json(&path, self)
    }
}

fn todo_path() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("todo.json"))
}

// Unix-only: these tests isolate by redirecting `$HOME`, which
// `dirs::home_dir()` honours on unix but ignores on Windows (profile API) —
// running them there would read and write the real `%USERPROFILE%\.cc-hub`.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_util::with_temp_home;

    #[test]
    fn add_persists_round_trip() {
        with_temp_home(|| {
            let mut t = TodoList::load();
            assert!(t.is_empty());
            assert_eq!(t.add("buy milk"), Some(0));
            assert_eq!(t.add("  ship PR  "), Some(1));
            let reloaded = TodoList::load();
            assert_eq!(reloaded.len(), 2);
            assert_eq!(reloaded.items()[0].text, "buy milk");
            // Whitespace is trimmed on the way in.
            assert_eq!(reloaded.items()[1].text, "ship PR");
            assert!(!reloaded.items()[1].done);
        });
    }

    #[test]
    fn empty_input_is_ignored() {
        with_temp_home(|| {
            let mut t = TodoList::load();
            assert_eq!(t.add("   "), None);
            assert!(t.is_empty());
        });
    }

    #[test]
    fn toggle_persists() {
        with_temp_home(|| {
            let mut t = TodoList::load();
            t.add("task");
            t.toggle(0);
            assert!(TodoList::load().items()[0].done);
            t.toggle(0);
            assert!(!TodoList::load().items()[0].done);
            // Out-of-range toggle is a no-op.
            t.toggle(9);
            assert_eq!(TodoList::load().len(), 1);
        });
    }

    #[test]
    fn remove_persists() {
        with_temp_home(|| {
            let mut t = TodoList::load();
            t.add("a");
            t.add("b");
            t.remove(0);
            let reloaded = TodoList::load();
            assert_eq!(reloaded.len(), 1);
            assert_eq!(reloaded.items()[0].text, "b");
        });
    }

    #[test]
    fn clear_completed_drops_done_items() {
        with_temp_home(|| {
            let mut t = TodoList::load();
            t.add("a");
            t.add("b");
            t.add("c");
            t.toggle(0); // a done
            t.toggle(2); // c done
            assert_eq!(t.clear_completed(), 2);
            let reloaded = TodoList::load();
            assert_eq!(reloaded.len(), 1);
            assert_eq!(reloaded.items()[0].text, "b");
            // Nothing left to clear → no-op.
            assert_eq!(TodoList::load().clear_completed(), 0);
        });
    }

    #[test]
    fn missing_file_yields_empty() {
        with_temp_home(|| {
            assert!(TodoList::load().is_empty());
        });
    }
}
