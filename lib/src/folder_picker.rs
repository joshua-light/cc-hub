//! Simple directory browser used when spawning a new tmux session from a
//! user-chosen path. Shows the subdirectories of `current_dir` and lets the
//! caller descend, ascend, or pick the current dir itself.

use std::fs;
use std::path::{Path, PathBuf};

pub struct FolderPicker {
    pub current_dir: PathBuf,
    pub entries: Vec<String>,
    pub selection: usize,
}

impl FolderPicker {
    pub fn new(start: PathBuf) -> Self {
        let mut picker = Self {
            current_dir: start,
            entries: Vec::new(),
            selection: 0,
        };
        picker.reload();
        picker
    }

    pub fn reload(&mut self) {
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
        let Some(name) = self.entries.get(self.selection) else {
            return;
        };
        self.current_dir = self.current_dir.join(name);
        self.reload();
    }

    pub fn ascend(&mut self) {
        let Some(parent) = self.current_dir.parent().map(Path::to_path_buf) else {
            return;
        };
        let prior = self.current_dir.file_name().map(|s| s.to_string_lossy().into_owned());
        self.current_dir = parent;
        self.reload();
        if let Some(name) = prior {
            if let Some(idx) = self.entries.iter().position(|e| e == &name) {
                self.selection = idx;
            }
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
