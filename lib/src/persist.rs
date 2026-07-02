//! Shared persistence helper for the JSON state files under `~/.cc-hub/`
//! (to-do list, bookmarks, …). Writes go through a tempfile + rename so a
//! crash mid-write can't leave a torn file behind.

use serde::Serialize;
use std::fs;
use std::io::{self, Write};
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
    // fsync the tempfile's bytes before the rename. `fs::write` alone leaves
    // the data in the page cache: with delayed allocation a power loss can
    // persist the rename while the bytes are still buffered, leaving a
    // zero-length file — the exact torn write the rename is meant to prevent.
    // Mirrors the atomic writer in spawn.rs::ensure_path_trusted.
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(raw.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}
