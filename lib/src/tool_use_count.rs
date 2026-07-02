//! In-memory incremental cache for transcript tool-use counts.
//!
//! Counting `tool_use` / `toolCall` blocks across whole orchestrator
//! transcripts on every scan tick is wasteful — these files grow into
//! the megabytes. We cache the last seen file size + count per path:
//! on a second hit, if the file hasn't grown we return the cached count
//! immediately; if it grew, we seek to the old size and only count the
//! suffix; if it shrank (rewrite), we recount from scratch.

use crate::{conversation, pi_conversation};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug)]
struct Cached {
    /// File length observed at last update — used to detect shrink
    /// (rotation/rewrite) and skip work when nothing changed.
    size: u64,
    /// Byte offset just past the last newline we processed. Always at a
    /// line boundary, so the next incremental read can `seek` here and
    /// count fresh lines without a discard step.
    clean_offset: u64,
    count: u64,
}

#[derive(Clone, Copy)]
enum Kind {
    Claude,
    Pi,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, Cached>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Cached>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Evict cached counts for transcripts not in `live`. Call once per scan tick
/// with the set of JSONL paths that surfaced, so this path-keyed cache can't
/// grow one entry per JSONL ever scanned for the whole process lifetime.
/// Mirrors [`conversation::retain_cached`].
pub fn retain_cached(live: &HashSet<PathBuf>) {
    if let Ok(mut guard) = cache().lock() {
        guard.retain(|path, _| live.contains(path));
    }
}

/// Cumulative `tool_use` count for a Claude JSONL transcript. Returns 0
/// when the file is missing or unreadable.
pub fn count_claude(path: &Path) -> u64 {
    count_with_kind(path, Kind::Claude)
}

/// Cumulative `toolCall` count for a Pi JSONL transcript. Returns 0 when
/// the file is missing or unreadable.
pub fn count_pi(path: &Path) -> u64 {
    count_with_kind(path, Kind::Pi)
}

fn count_with_kind(path: &Path, kind: Kind) -> u64 {
    let Ok(meta) = std::fs::metadata(path) else {
        return 0;
    };
    let size = meta.len();

    let Ok(mut guard) = cache().lock() else {
        return 0;
    };
    let prev = guard.get(path).copied();

    let (count, clean_offset) = match prev {
        Some(c) if c.size == size => return c.count,
        // Detect shrink against the cached *size*, not the clean offset: a
        // rewrite that shrinks the file into `[clean_offset, old_size)` still
        // has `size >= clean_offset`, so comparing to clean_offset would
        // misread it as append-only growth and corrupt the count. Only a file
        // that actually grew past its cached size gets the incremental path.
        Some(c) if size > c.size => {
            // Append-only growth: pick up at the last clean line boundary
            // and tally only complete new lines.
            let Some((delta, new_offset)) = count_from(path, c.clean_offset, kind) else {
                return c.count;
            };
            (c.count.saturating_add(delta), new_offset)
        }
        // No prior entry, or the file shrank (rewrite/rotation): recount
        // from scratch.
        _ => match count_from(path, 0, kind) {
            Some((n, new_offset)) => (n, new_offset),
            None => return 0,
        },
    };

    guard.insert(
        path.to_path_buf(),
        Cached {
            size,
            clean_offset,
            count,
        },
    );
    count
}

/// Count tool_uses starting from `start` (which must be at a line boundary
/// or 0). Returns `(count, new_clean_offset)` where the new offset is the
/// byte position just past the last `\n` read — guaranteed to be at a line
/// boundary so the next incremental call can resume there without re-reading.
fn count_from(path: &Path, start: u64, kind: Kind) -> Option<(u64, u64)> {
    let mut file = File::open(path).ok()?;
    if start > 0 && file.seek(SeekFrom::Start(start)).is_err() {
        return None;
    }
    let mut reader = BufReader::new(file);
    let mut count = 0u64;
    let mut consumed: u64 = 0;
    let mut clean_offset = start;
    loop {
        let mut buf = String::new();
        let n = match reader.read_line(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        consumed += n as u64;
        let ends_with_newline = buf.ends_with('\n');
        if ends_with_newline {
            // Only count complete lines — a partial trailing line might
            // still grow into a tool_use we'd otherwise miss/double-count.
            let line_count = match kind {
                Kind::Claude => conversation::count_tool_uses_in_reader(buf.as_bytes()),
                Kind::Pi => pi_conversation::count_tool_uses_in_reader(buf.as_bytes()),
            };
            count = count.saturating_add(line_count as u64);
            clean_offset = start + consumed;
        }
    }
    Some((count, clean_offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::sync::Mutex;

    // The count cache is a process-global static. Tests that depend on its
    // persistence across calls (or evict it) must not interleave with each
    // other, so every cache-touching test holds this lock for its duration.
    static CACHE_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn write_assistant_with_tool_uses(path: &Path, n: usize) {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open");
        for _ in 0..n {
            let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"x","name":"Read"}]}}"#;
            writeln!(f, "{}", line).expect("write");
        }
    }

    fn fresh_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "cchub-tool-use-cache-{}-{}.jsonl",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn first_call_counts_everything() {
        let _g = CACHE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = fresh_path("first");
        write_assistant_with_tool_uses(&p, 3);
        assert_eq!(count_claude(&p), 3);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn cached_value_used_when_size_unchanged() {
        let _g = CACHE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = fresh_path("cached");
        write_assistant_with_tool_uses(&p, 2);
        assert_eq!(count_claude(&p), 2);

        // Surreptitiously rewrite the file with different content but the
        // exact same byte size. The cache must NOT re-read — it should
        // trust the cached count and return 2 even though the new content
        // has a different number of tool_uses.
        let original = std::fs::read(&p).expect("read");
        let original_len = original.len();
        let mut replacement = String::new();
        replacement.push_str(r#"{"type":"user"}"#);
        replacement.push('\n');
        while replacement.len() < original_len {
            replacement.push(' ');
        }
        replacement.truncate(original_len);
        std::fs::write(&p, &replacement).expect("rewrite");
        assert_eq!(std::fs::metadata(&p).unwrap().len() as usize, original_len);

        // Cache hit (same size) — returns the original cached count.
        assert_eq!(count_claude(&p), 2);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn appended_lines_grow_count_incrementally() {
        let _g = CACHE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = fresh_path("append");
        write_assistant_with_tool_uses(&p, 4);
        assert_eq!(count_claude(&p), 4);

        write_assistant_with_tool_uses(&p, 3);
        assert_eq!(count_claude(&p), 7);

        // Append a non-tool entry — count should not grow.
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(
            f,
            r#"{{"type":"user","message":{{"role":"user","content":"hi"}}}}"#
        )
        .unwrap();
        assert_eq!(count_claude(&p), 7);

        write_assistant_with_tool_uses(&p, 2);
        assert_eq!(count_claude(&p), 9);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn shrink_recounts_from_scratch() {
        let _g = CACHE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = fresh_path("shrink");
        write_assistant_with_tool_uses(&p, 2);
        let size = std::fs::metadata(&p).unwrap().len();

        // Simulate a cache entry from when the file was larger, with a clean
        // offset *below* the current (shrunk) size. The old buggy guard
        // (`size >= clean_offset`) would treat this as append-only growth and
        // keep the stale count 99; the fix compares against the cached size,
        // sees a shrink, and recounts from scratch → 2.
        {
            let mut guard = cache().lock().unwrap();
            guard.insert(
                p.clone(),
                Cached {
                    size: size + 1000,
                    clean_offset: size.saturating_sub(10),
                    count: 99,
                },
            );
        }

        assert_eq!(count_claude(&p), 2);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn retain_cached_evicts_unlisted_paths() {
        let _g = CACHE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = fresh_path("retain");
        write_assistant_with_tool_uses(&p, 1);
        assert_eq!(count_claude(&p), 1);
        assert!(cache().lock().unwrap().contains_key(&p));

        // Path absent from the live set → evicted.
        retain_cached(&HashSet::new());
        assert!(!cache().lock().unwrap().contains_key(&p));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn missing_file_returns_zero() {
        let mut p = std::env::temp_dir();
        p.push("cchub-tool-use-cache-does-not-exist.jsonl");
        let _ = std::fs::remove_file(&p);
        assert_eq!(count_claude(&p), 0);
    }
}
