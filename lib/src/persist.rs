//! Shared persistence helper for the JSON state files under `~/.cc-hub/`
//! (to-do list, bookmarks, …). Writes go through a tempfile + rename so a
//! crash mid-write can't leave a torn file behind.

use serde::Serialize;
use std::fs;
use std::io;
use std::path::Path;

/// Serialise `value` as pretty JSON and atomically replace `path` with it,
/// creating parent directories as needed. The tempfile is namespaced by pid
/// so two instances replacing the same file don't trip over each other's
/// staging file (last rename still wins, but neither write tears).
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&tmp, raw)?;
    fs::rename(&tmp, path)
}
