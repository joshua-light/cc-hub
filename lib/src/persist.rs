//! Persistence helpers for the state files under `~/.cc-hub/`: atomic
//! writes (tempfile, fsync, rename) so a crash mid-write can't leave a torn
//! file, and the sidecar flock that serializes read-modify-write cycles.

use serde::Serialize;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Serialise `value` as pretty JSON and atomically replace `path` with it,
/// creating parent directories as needed. The tempfile is namespaced by pid
/// so two instances replacing the same file don't trip over each other's
/// staging file (last rename still wins, but neither write tears).
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let raw = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    write_atomic(path, raw.as_bytes())
}

/// Atomically replace `path` with `bytes`, creating parent directories as
/// needed. See [`save_json`] for why the tempfile is pid-namespaced.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    // fsync the tempfile's bytes before the rename. `fs::write` alone leaves
    // the data in the page cache: with delayed allocation a power loss can
    // persist the rename while the bytes are still buffered, leaving a
    // zero-length file — the exact torn write the rename is meant to prevent.
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// Wall-clock unix seconds for on-disk timestamps; 0 on a pre-epoch clock.
pub fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Open the sidecar lock file at `path`, creating it if needed, and block
/// until this process holds an exclusive flock on it. The lock lasts as long
/// as the returned `File`, so bind it to a named variable, not `_`.
pub fn lock_exclusive(path: &Path) -> io::Result<fs::File> {
    use fs2::FileExt;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    lock.lock_exclusive()?;
    Ok(lock)
}
