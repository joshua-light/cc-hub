use super::paths::{claude_dir, encode_path, find_jsonl, projects_dir};
use crate::conversation;
use crate::models::{short_sid, RawSession};
use log::debug;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// session_id → latest `/clear` timestamp (ms), parsed from the tail of
/// `history.jsonl`.
pub(super) type ClearMap = HashMap<String, u64>;

/// The history file's cache key: its `(mtime, len)`. A change in either forces
/// a re-read.
type ClearsKey = (PathBuf, SystemTime, u64);

/// Cache slot: the key the map was parsed at, plus the shared map itself.
type ClearsSlot = Option<(ClearsKey, Arc<ClearMap>)>;

/// Process-global cache of the parsed `/clear` map, keyed on the history
/// file's `(mtime, len)`. `None` means "not yet computed / file absent last
/// tick"; the stat itself runs every tick (cheap), but the 128 KiB tail read +
/// parse only re-runs when the stat key changes.
fn clears_cache() -> &'static Mutex<ClearsSlot> {
    static CACHE: OnceLock<Mutex<ClearsSlot>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Counts how many times the history tail was actually read+parsed (a cache
/// miss). Tests assert this does not advance across a second call with an
/// unchanged `(mtime, len)`.
#[cfg(test)]
static CLEARS_READS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Test hook: total history reads (cache misses) so far.
#[cfg(test)]
fn clears_read_count() -> u64 {
    CLEARS_READS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Read /clear events from the tail of ~/.claude/history.jsonl.
/// Returns session_id → latest clear timestamp (ms).
///
/// Memoized on the history file's `(mtime, len)`: the scan calls this every
/// tick, but the 128 KiB tail read only re-runs when the file changes. A
/// missing/unstattable file resolves to an empty map without any read — but the
/// stat still runs each tick so a freshly-created history is picked up promptly.
pub(super) fn read_clears_from_history() -> Arc<ClearMap> {
    let path = match claude_dir() {
        Some(d) => d.join("history.jsonl"),
        None => return Arc::new(ClearMap::new()),
    };

    // Stat every tick (cheap); only the read+parse below is gated on the key.
    let key = std::fs::metadata(&path)
        .ok()
        .and_then(|m| Some((path.clone(), m.modified().ok()?, m.len())));

    let Some(key) = key else {
        // Missing/unstattable file: empty map. Cheap to rebuild if it appears.
        return Arc::new(ClearMap::new());
    };

    {
        let cache = clears_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((cached_key, clears)) = cache.as_ref() {
            if *cached_key == key {
                return Arc::clone(clears);
            }
        }
    }

    let clears = Arc::new(read_clears_uncached(&path));
    #[cfg(test)]
    CLEARS_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut cache = clears_cache().lock().unwrap_or_else(|e| e.into_inner());
    *cache = Some((key, Arc::clone(&clears)));
    clears
}

/// The uncached 128 KiB tail read + parse of `history.jsonl`.
fn read_clears_uncached(path: &Path) -> ClearMap {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Default::default(),
    };

    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut reader = std::io::BufReader::new(file);
    let tail_bytes = 128 * 1024;
    if len > tail_bytes {
        use std::io::{Seek, SeekFrom};
        let _ = reader.seek(SeekFrom::Start(len - tail_bytes));
        let mut discard = String::new();
        let _ = std::io::BufRead::read_line(&mut reader, &mut discard);
    }

    let mut clears = HashMap::new();
    for line in std::io::BufRead::lines(reader) {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if !line.contains("/clear") {
            continue;
        }
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&line) {
            if obj.get("display").and_then(|d| d.as_str()) == Some("/clear") {
                if let (Some(sid), Some(ts)) = (
                    obj.get("sessionId").and_then(|s| s.as_str()),
                    obj.get("timestamp").and_then(|t| t.as_u64()),
                ) {
                    let entry = clears.entry(sid.to_string()).or_insert(0u64);
                    if ts > *entry {
                        *entry = ts;
                    }
                }
            }
        }
    }
    clears
}

/// Resolve the JSONL path for every alive session.
///
/// After `/clear`, Claude Code forks into a new JSONL under a fresh session
/// ID but leaves the session metadata file pointing at the OLD sid — whose
/// JSONL is frozen at pre-clear content. To find the live file we follow
/// the `/clear` chain: each clear event has a timestamp that lines up
/// (within a few ms) with the first entry of the next JSONL, so we walk
/// orphan JSONLs in the same project dir until we reach an uncleared sid.
///
/// Resolution strategy:
///   1. Non-cleared sessions → use their own JSONL.
///   2. Cleared sessions → follow the `/clear` chain. If the chain breaks
///      (no orphan within the timestamp window), fall back to the session's
///      own JSONL so we at least show its pre-clear state instead of nothing.
pub(super) fn resolve_jsonl_paths(
    sessions: &[RawSession],
    clears: &HashMap<String, u64>,
    claimed: &HashSet<String>,
) -> HashMap<String, Option<PathBuf>> {
    let mut result: HashMap<String, Option<PathBuf>> = HashMap::new();
    let mut orphan_index: HashMap<String, OrphanIndex> = HashMap::new();

    for raw in sessions {
        let sid_short = short_sid(&raw.session_id);
        let path = if let Some(&clear_ts) = clears.get(&raw.session_id) {
            let chained = orphan_index
                .entry(raw.cwd.clone())
                .or_insert_with(|| OrphanIndex::build(&raw.cwd, claimed))
                .follow_chain(&raw.session_id, clears);
            let resolved = chained.or_else(|| find_jsonl(&raw.cwd, &raw.session_id));
            debug!(
                "resolve sid={} (cleared at {}) → {}",
                sid_short,
                clear_ts,
                resolved
                    .as_ref()
                    .map_or("not found".to_string(), |p| p.display().to_string())
            );
            resolved
        } else {
            let direct = find_jsonl(&raw.cwd, &raw.session_id);
            debug!(
                "resolve sid={} direct → {}",
                sid_short,
                direct.as_ref().map_or("not found", |_| "found")
            );
            direct
        };
        result.insert(raw.session_id.clone(), path);
    }

    result
}

/// Index of orphan JSONLs in a project directory, keyed by first-entry
/// timestamp for fast lookup during /clear chain resolution.
struct OrphanIndex {
    /// (first_entry_timestamp_ms, session_id, path)
    entries: Vec<(u64, String, PathBuf)>,
}

impl OrphanIndex {
    fn build(cwd: &str, claimed: &HashSet<String>) -> Self {
        let proj_dir = match projects_dir().map(|p| p.join(encode_path(cwd))) {
            Some(d) => d,
            None => {
                return Self {
                    entries: Vec::new(),
                }
            }
        };
        let dir_entries = match std::fs::read_dir(&proj_dir) {
            Ok(e) => e,
            Err(_) => {
                return Self {
                    entries: Vec::new(),
                }
            }
        };

        let mut entries = Vec::new();
        for entry in dir_entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            let sid = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            if claimed.contains(&sid) {
                continue;
            }
            if let Some(ts) = read_first_timestamp(&path) {
                entries.push((ts, sid, path));
            }
        }
        Self { entries }
    }

    /// Find the orphan whose first entry is within `max_delta` ms of `clear_ts`.
    fn find_by_clear_ts(&self, clear_ts: u64) -> Option<(&str, &Path)> {
        let max_delta = 30_000u64;
        let mut best: Option<(usize, u64)> = None;
        for (i, (first_ts, _, _)) in self.entries.iter().enumerate() {
            if *first_ts < clear_ts {
                continue;
            }
            let delta = first_ts - clear_ts;
            if delta > max_delta {
                continue;
            }
            if best.as_ref().is_none_or(|(_, d)| delta < *d) {
                best = Some((i, delta));
            }
        }
        best.map(|(i, _)| {
            let (_, ref sid, ref path) = self.entries[i];
            (sid.as_str(), path.as_path())
        })
    }

    /// Follow the /clear chain starting from `start_sid` until we reach a
    /// session ID that was NOT cleared.  Returns the JSONL path at the end
    /// of the chain, or None if the chain is broken.
    fn follow_chain(&self, start_sid: &str, clears: &HashMap<String, u64>) -> Option<PathBuf> {
        let mut current_sid = start_sid.to_string();
        let mut visited = HashSet::new();

        loop {
            let clear_ts = match clears.get(&current_sid) {
                Some(&ts) => ts,
                None => {
                    // current_sid was NOT cleared — it's the head of the chain.
                    // Find its JSONL path in our index.
                    return self
                        .entries
                        .iter()
                        .find(|(_, sid, _)| sid == &current_sid)
                        .map(|(_, _, p)| p.clone());
                }
            };

            visited.insert(current_sid.clone());

            match self.find_by_clear_ts(clear_ts) {
                Some((next_sid, _)) => {
                    if visited.contains(next_sid) {
                        return None; // cycle
                    }
                    current_sid = next_sid.to_string();
                }
                None => return None, // broken chain
            }
        }
    }
}

/// Read the first timestamp (epoch ms) from a JSONL file.
/// Only reads the first ~4 KB to keep it fast.
fn read_first_timestamp(path: &Path) -> Option<u64> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    // Only check first ~20 lines (well within 4 KB).
    for line in reader.lines().take(20) {
        let line = line.ok()?;
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&line) {
            if let Some(ts) = obj.get("timestamp") {
                return conversation::parse_timestamp_ms(ts);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::HOME_TEST_LOCK;
    use std::fs;

    fn with_temp_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        // CLAUDE_CONFIG_DIR overrides the HOME-derived layout in
        // platform::paths — leaving it set (e.g. when the test runner itself
        // was launched with --claude-config-dir) would point the scanner at
        // the real config dir instead of the temp home.
        let prev_cfg = std::env::var_os("CLAUDE_CONFIG_DIR");
        std::env::remove_var("CLAUDE_CONFIG_DIR");
        std::env::set_var("HOME", home.path());
        let out = f(home.path());
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_cfg {
            Some(v) => std::env::set_var("CLAUDE_CONFIG_DIR", v),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
        out
    }

    // The history read is memoized on the file's (mtime, len): a second scan
    // tick with an unchanged file is served from cache (no re-read), while
    // appending a /clear line (which bumps both mtime and len) forces a
    // re-read that surfaces the new event.
    #[test]
    fn clears_cache_reads_once_until_history_changes() {
        with_temp_home(|home| {
            let dir = home.join(".claude");
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join("history.jsonl");
            fs::write(
                &path,
                "{\"display\":\"/clear\",\"sessionId\":\"s1\",\"timestamp\":1000}\n",
            )
            .unwrap();

            let before = clears_read_count();
            let first = read_clears_from_history();
            assert_eq!(
                clears_read_count(),
                before + 1,
                "first call is a cache miss → one read"
            );
            assert_eq!(first.get("s1"), Some(&1000));

            let second = read_clears_from_history();
            assert_eq!(
                clears_read_count(),
                before + 1,
                "unchanged file → second call served from cache, no re-read"
            );
            assert_eq!(second.get("s1"), Some(&1000));

            // Append a newer /clear for a second session. Appending grows the
            // file (len changes) and rewrites mtime, invalidating the key.
            let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
            std::io::Write::write_all(
                &mut f,
                b"{\"display\":\"/clear\",\"sessionId\":\"s2\",\"timestamp\":2000}\n",
            )
            .unwrap();
            drop(f);
            // Guard against same-second mtime granularity: force a distinct
            // modified time so the (mtime, len) key definitely differs.
            let bumped = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(bumped)
                .unwrap();

            let third = read_clears_from_history();
            assert_eq!(clears_read_count(), before + 2, "changed file → re-read");
            assert_eq!(third.get("s1"), Some(&1000));
            assert_eq!(third.get("s2"), Some(&2000));
        });
    }
}
