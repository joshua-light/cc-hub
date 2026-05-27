//! Simple directory browser used when spawning a new tmux session from a
//! user-chosen path. Shows the subdirectories of `current_dir` and lets the
//! caller descend, ascend, or pick the current dir itself.
//!
//! Also supports a flat [`PickerMode::Bookmarks`] mode where `entries`
//! holds absolute paths drawn from [`crate::bookmarks::Bookmarks`]; in that
//! mode descend/ascend are no-ops and picking just selects the entry.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerMode {
    Browse,
    Bookmarks,
}

pub struct FolderPicker {
    pub current_dir: PathBuf,
    pub entries: Vec<String>,
    pub selection: usize,
    pub mode: PickerMode,
}

impl FolderPicker {
    pub fn new(start: PathBuf) -> Self {
        let mut picker = Self {
            current_dir: start,
            entries: Vec::new(),
            selection: 0,
            mode: PickerMode::Browse,
        };
        picker.reload();
        picker
    }

    /// Construct a picker pre-populated with `bookmarks` as a flat list.
    /// `current_dir` is set to the deepest common ancestor of the entries
    /// (or `/` when empty) just so legacy callers that read it have
    /// something sensible — it has no effect on rendering in this mode.
    pub fn new_bookmarks(bookmarks: Vec<PathBuf>) -> Self {
        let current_dir = bookmarks
            .first()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/"));
        let entries = bookmarks
            .into_iter()
            .map(|p| p.display().to_string())
            .collect();
        Self {
            current_dir,
            entries,
            selection: 0,
            mode: PickerMode::Bookmarks,
        }
    }

    pub fn reload(&mut self) {
        if self.mode == PickerMode::Bookmarks {
            // Bookmarks entries are authoritative; reload would wipe them.
            return;
        }
        self.entries = list_subdirs(&self.current_dir);
        self.selection = 0;
    }

    pub fn move_up(&mut self) {
        if self.selection > 0 {
            self.selection -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selection + 1 < self.entries.len() {
            self.selection += 1;
        }
    }

    pub fn descend(&mut self) {
        if self.mode == PickerMode::Bookmarks {
            return;
        }
        let Some(name) = self.entries.get(self.selection) else {
            return;
        };
        self.current_dir = self.current_dir.join(name);
        self.reload();
    }

    pub fn ascend(&mut self) {
        if self.mode == PickerMode::Bookmarks {
            return;
        }
        let Some(parent) = self.current_dir.parent().map(Path::to_path_buf) else {
            return;
        };
        let prior = self
            .current_dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned());
        self.current_dir = parent;
        self.reload();
        if let Some(name) = prior {
            if let Some(idx) = self.entries.iter().position(|e| e == &name) {
                self.selection = idx;
            }
        }
    }

    /// Absolute path of the currently-highlighted entry. In Browse mode
    /// that's `current_dir/entry`; in Bookmarks mode the entry already is
    /// an absolute path. Returns `None` when the list is empty.
    pub fn selected_path(&self) -> Option<PathBuf> {
        let entry = self.entries.get(self.selection)?;
        Some(match self.mode {
            PickerMode::Browse => self.current_dir.join(entry),
            PickerMode::Bookmarks => PathBuf::from(entry),
        })
    }

    /// Remove the highlighted entry from `entries` and clamp the cursor.
    /// Only meaningful in Bookmarks mode — keeps the view in sync with the
    /// backing store after a toggle-off without rebuilding the picker.
    pub fn remove_selected(&mut self) {
        if self.selection >= self.entries.len() {
            return;
        }
        self.entries.remove(self.selection);
        if self.selection >= self.entries.len() && self.selection > 0 {
            self.selection -= 1;
        }
    }
}

fn list_subdirs(path: &Path) -> Vec<String> {
    let Ok(read) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut out: Vec<String> = read
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    out.sort_by_key(|s| s.to_lowercase());
    out
}
