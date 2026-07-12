//! Per-directory listing cache for the orphan / inactive-session walks.
//!
//! Both the Claude orphan walk ([`crate::scanner::scan_orphan_jsonls`]) and the
//! Pi inactive walk ([`crate::pi_scanner::scan_inactive_sessions`]) `read_dir`
//! and `stat` every `*.jsonl` under their session tree on every scan tick.
//! Those trees only change when a session writes a new transcript, so re-listing
//! a directory whose mtime is unchanged is pure waste.
//!
//! [`list_jsonl_dir`] returns the cached listing when the directory's mtime is
//! unchanged AND the entry was listed within `ttl`; otherwise it re-lists. A new
//! file bumps the directory's mtime, so it invalidates the entry immediately;
//! the TTL only bounds how stale an *otherwise-unchanged* listing may get (files
//! rewritten in place don't touch the dir mtime).
//!
//! Lives in its own module rather than inside `scanner.rs` so both scanners can
//! share one cache without either reaching into the other's internals, keeping
//! the mtime + TTL + retain-by-visited invariants in one place.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// A `*.jsonl` file and its last-modified time, as observed when the containing
/// directory was listed. Callers derive per-file age from this cached mtime.
pub type FileEntry = (PathBuf, SystemTime);

/// One directory's cached listing.
struct CacheEntry {
    /// The directory's own mtime when it was listed. A changed mtime (a new or
    /// removed file) invalidates the entry regardless of TTL.
    dir_mtime: SystemTime,
    /// When the listing was taken, for TTL comparison.
    listed_at: Instant,
    /// The `*.jsonl` files in the directory, each with its mtime. Wrapped in an
    /// `Arc` so cache hits hand out a cheap clone.
    files: Arc<Vec<FileEntry>>,
}

type DirCache = HashMap<PathBuf, CacheEntry>;

fn cache() -> &'static Mutex<DirCache> {
    static CACHE: OnceLock<Mutex<DirCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Counts how many times a directory was actually `read_dir`+stat'd (a cache
/// miss). Tests assert it does not advance across a second call within TTL.
#[cfg(test)]
static DIR_RELISTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Test hook: total directory re-lists (cache misses) so far.
#[cfg(test)]
pub fn relist_count() -> u64 {
    DIR_RELISTS.load(std::sync::atomic::Ordering::Relaxed)
}

/// List the `*.jsonl` files (with mtimes) directly under `dir`, memoized on the
/// directory's mtime and bounded by `ttl`.
///
/// Returns the cached listing when the directory's mtime is unchanged since the
/// last listing AND that listing is younger than `ttl`; otherwise re-lists and
/// updates the cache. Files whose mtime can't be read are skipped, matching the
/// uncached walks' `and_then(modified)` filter. A missing/unreadable directory
/// yields an empty listing (and is cached against its current dir-mtime, so a
/// dir that later appears re-lists on the next mtime change).
pub fn list_jsonl_dir(dir: &Path, ttl: Duration) -> Arc<Vec<FileEntry>> {
    let dir_mtime = std::fs::metadata(dir)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);

    {
        let cache = cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = cache.get(dir) {
            if entry.dir_mtime == dir_mtime && entry.listed_at.elapsed() < ttl {
                return Arc::clone(&entry.files);
            }
        }
    }

    let files = Arc::new(list_jsonl_uncached(dir));
    #[cfg(test)]
    DIR_RELISTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.insert(
        dir.to_path_buf(),
        CacheEntry {
            dir_mtime,
            listed_at: Instant::now(),
            files: Arc::clone(&files),
        },
    );
    files
}

/// Uncached `read_dir` of `dir`, collecting `*.jsonl` files with their mtimes.
fn list_jsonl_uncached(dir: &Path) -> Vec<FileEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(mtime) = path.metadata().ok().and_then(|m| m.modified().ok()) else {
            continue;
        };
        out.push((path, mtime));
    }
    out
}

/// Evict cache entries for directories not present in `visited` this scan.
/// Mirrors the retain-by-visited eviction in [`crate::conversation::retain_cached`];
/// call once per scan with the set of directories actually listed this tick so
/// the cache doesn't retain directories that aged out or were removed.
pub fn retain(visited: &std::collections::HashSet<PathBuf>) {
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|k, _| visited.contains(k));
}

/// Like [`retain`], but only considers (and possibly evicts) cache entries
/// under `root`. Entries outside `root` are left untouched. This lets the
/// Claude and Pi scanners each evict their own subtree (`~/.claude/projects`
/// vs `~/.pi/agent/sessions`) without clobbering the other's entries in the
/// shared cache.
pub fn retain_under(root: &Path, visited: &std::collections::HashSet<PathBuf>) {
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|k, _| !k.starts_with(root) || visited.contains(k));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Serializes the relist-counter tests: the counter and cache are
    /// process-global, so concurrent runs would race the "does not advance"
    /// assertions.
    static RELIST_LOCK: StdMutex<()> = StdMutex::new(());

    fn write_jsonl(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"{}\n").unwrap();
    }

    #[test]
    fn served_from_cache_within_ttl_and_relists_on_new_file() {
        let _guard = RELIST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_jsonl(dir, "a.jsonl");

        let ttl = Duration::from_secs(30);
        let before = relist_count();

        let first = list_jsonl_dir(dir, ttl);
        assert_eq!(relist_count(), before + 1, "first call is a miss");
        assert_eq!(first.len(), 1);

        let second = list_jsonl_dir(dir, ttl);
        assert_eq!(
            relist_count(),
            before + 1,
            "unchanged dir within TTL → served from cache"
        );
        assert_eq!(second.len(), 1);

        // A new file bumps the dir mtime → immediate invalidation.
        write_jsonl(dir, "b.jsonl");
        let third = list_jsonl_dir(dir, ttl);
        assert_eq!(
            relist_count(),
            before + 2,
            "new file changes dir mtime → re-list"
        );
        assert_eq!(third.len(), 2);

        retain(&std::collections::HashSet::new());
    }

    #[test]
    fn ttl_expiry_forces_relist_even_with_unchanged_mtime() {
        let _guard = RELIST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_jsonl(dir, "a.jsonl");

        // A zero TTL means every call is stale → always re-lists, even though
        // the directory mtime never changes between calls.
        let zero = Duration::from_secs(0);
        let before = relist_count();
        let _ = list_jsonl_dir(dir, zero);
        assert_eq!(relist_count(), before + 1);
        let _ = list_jsonl_dir(dir, zero);
        assert_eq!(
            relist_count(),
            before + 2,
            "expired TTL forces a re-list despite unchanged mtime"
        );

        retain(&std::collections::HashSet::new());
    }

    #[test]
    fn retain_evicts_unvisited_dirs() {
        let _guard = RELIST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_jsonl(dir, "a.jsonl");

        let ttl = Duration::from_secs(30);
        let before = relist_count();
        let _ = list_jsonl_dir(dir, ttl);
        assert_eq!(relist_count(), before + 1);

        // Evict with an empty visited set → next access re-lists.
        retain(&std::collections::HashSet::new());
        let _ = list_jsonl_dir(dir, ttl);
        assert_eq!(
            relist_count(),
            before + 2,
            "evicted dir must be re-listed on next access"
        );

        retain(&std::collections::HashSet::new());
    }
}
