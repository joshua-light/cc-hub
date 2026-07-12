//! Mtime-keyed memoization of derived transcript state and immutable
//! first-user-message summaries.

use super::io::{read_jsonl_head, read_jsonl_tail_for_state};
use super::messages::{
    extract_first_user_message, extract_last_activity, extract_last_user_message, extract_metadata,
};
use super::state::{
    extract_context_tokens, extract_current_tool, extract_state_at, is_currently_thinking,
    CurrentTool,
};
use crate::models::SessionState;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// All per-tick state derived from a Claude JSONL transcript's tail window.
///
/// Bundling these lets the scan derive them once per `(path, mtime)` and reuse
/// the result across the whole pipeline instead of re-parsing 64 KiB–4 MiB
/// several times every scan tick. Cheap to clone (mostly `Option<String>` and a
/// small `CurrentTool`); cache hits hand out an `Arc` so even that is skipped.
#[derive(Clone, Debug)]
pub struct StateDerivation {
    pub state: SessionState,
    pub last_user_message: Option<String>,
    pub last_activity: Option<u64>,
    pub git_branch: Option<String>,
    pub model: Option<String>,
    pub version: Option<String>,
    pub current_tool: Option<CurrentTool>,
    pub is_thinking: bool,
    pub context_tokens: Option<u64>,
}

impl StateDerivation {
    /// Run the full extract pipeline over a tail window. Pure: no IO, no
    /// cache. `source` labels unknown-stop-reason warnings with the file.
    fn derive(entries: &[Value], source: &Path) -> Self {
        let (git_branch, model, version) = extract_metadata(entries);
        StateDerivation {
            state: extract_state_at(entries, Some(source)),
            last_user_message: extract_last_user_message(entries),
            last_activity: extract_last_activity(entries),
            git_branch,
            model,
            version,
            current_tool: extract_current_tool(entries),
            is_thinking: is_currently_thinking(entries),
            context_tokens: extract_context_tokens(entries),
        }
    }
}

/// Mtime-keyed map of JSONL path → last-derived state. Mirrors
/// `projects_scan::TaskStateCache`.
type StateCache = HashMap<PathBuf, (SystemTime, Arc<StateDerivation>)>;

/// Process-global mtime-keyed cache of derived transcript state. Keyed by the
/// JSONL's absolute path; value is `(mtime, derived)`. Every scan stat()s each
/// file and only re-reads + re-derives on an mtime change — otherwise it hands
/// back the `Arc` clone, short-circuiting the read_jsonl_tail_for_state +
/// extract_* pipeline entirely. Mirrors the mtime-keyed cache in
/// [`crate::projects_scan`] and the size-keyed one in [`crate::tool_use_count`].
fn state_cache() -> &'static Mutex<StateCache> {
    static CACHE: OnceLock<Mutex<StateCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Immutable first-user-message summary cache. The first user message of a
/// session never changes once written, so once derived for a path we never
/// re-read the head. Only successful extractions are stored: a fresh session's
/// JSONL exists for a beat before the first prompt is flushed, so a `None`
/// must stay uncached and be re-derived next tick (see
/// [`first_user_message_cached`]).
fn summary_cache() -> &'static Mutex<HashMap<PathBuf, String>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Counts how many times the derivation pipeline actually read+parsed a
/// transcript (i.e. a cache miss). Tests assert this does not advance across a
/// second call with an unchanged mtime.
static STATE_DERIVE_PARSES: AtomicU64 = AtomicU64::new(0);

/// Test/observability hook: total number of cache-missing derivations so far.
#[cfg(test)]
pub fn state_derive_parse_count() -> u64 {
    STATE_DERIVE_PARSES.load(Ordering::Relaxed)
}

/// Derived transcript state for `path`, memoized on `(path, mtime)`.
///
/// On a cache hit (file mtime unchanged since last tick) this returns the
/// cached `Arc` without touching the file beyond the `stat`. On a miss (new
/// entry or changed mtime) it re-reads the tail, re-derives, and updates the
/// entry. The result is byte-for-byte identical to calling
/// [`read_jsonl_tail_for_state`] + the `extract_*` family directly — the cache
/// is purely a memoization layer keyed on mtime.
///
/// Returns `None` only when the file is missing/unstattable (mirroring the
/// empty-Vec fallback of the uncached readers, which all degrade to defaults).
pub fn derive_state_cached(path: &Path) -> Option<Arc<StateDerivation>> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;

    {
        let cache = state_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((cached_mtime, derived)) = cache.get(path) {
            if *cached_mtime == mtime {
                return Some(Arc::clone(derived));
            }
        }
    }

    // Miss: read + derive outside the lock so concurrent spawn_blocking scans
    // of *other* paths aren't serialized behind this file's IO.
    let entries = read_jsonl_tail_for_state(path);
    STATE_DERIVE_PARSES.fetch_add(1, Ordering::Relaxed);
    let derived = Arc::new(StateDerivation::derive(&entries, path));

    let mut cache = state_cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.insert(path.to_path_buf(), (mtime, Arc::clone(&derived)));
    Some(derived)
}

/// Head window for summary extraction. Covers the meta entries (`mode`,
/// `permission-mode`, `file-history-snapshot`) plus any typical first prompt.
const SUMMARY_HEAD_BYTES: u64 = 4096;
/// Fallback window when the first user line straddles the 4KB boundary —
/// `read_jsonl_head` drops a partial last line, so a long pasted prompt
/// needs a window that contains it whole.
const SUMMARY_HEAD_RETRY_BYTES: u64 = 256 * 1024;

/// First-user-message summary for `path`, memoized permanently per path once
/// a message exists.
///
/// Unlike [`derive_state_cached`] this is NOT keyed on mtime: the first user
/// message is immutable for the life of a session, so once read we never touch
/// the head of the file again. A `None` extraction is NOT cached, though —
/// Claude Code flushes the meta lines (`mode`, `permission-mode`,
/// `file-history-snapshot`) a beat before the first `user` line, so a scan
/// tick can observe the JSONL before the prompt lands. Caching that `None`
/// would leave the session summary-less (and therefore Haiku-title-less) for
/// the rest of the process; instead the head is re-read each tick until the
/// message appears. Stale entries are reclaimed by [`retain_cached`] when the
/// path disappears.
pub fn first_user_message_cached(path: &Path) -> Option<String> {
    {
        let cache = summary_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = cache.get(path) {
            return Some(cached.clone());
        }
    }
    let head = read_jsonl_head(path, SUMMARY_HEAD_BYTES);
    let summary = extract_first_user_message(&head).or_else(|| {
        // A long first prompt can start inside the 4KB window but extend past
        // it, in which case read_jsonl_head discards it as a partial line —
        // retry once with a window big enough for any realistic prompt.
        if std::fs::metadata(path).is_ok_and(|m| m.len() > SUMMARY_HEAD_BYTES) {
            let head = read_jsonl_head(path, SUMMARY_HEAD_RETRY_BYTES);
            extract_first_user_message(&head)
        } else {
            None
        }
    })?;
    let mut cache = summary_cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.insert(path.to_path_buf(), summary.clone());
    Some(summary)
}

/// Evict cache entries for transcripts not present in `visited` this scan
/// (sessions that aged out of the window, deleted files). Mirrors the
/// `retain`-by-visited-set eviction in [`crate::projects_scan`]. Call once at
/// the end of a scan with the set of paths actually parsed this tick.
pub fn retain_cached(visited: &HashSet<PathBuf>) {
    {
        let mut cache = state_cache().lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|k, _| visited.contains(k));
    }
    {
        let mut cache = summary_cache().lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|k, _| visited.contains(k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::test_util::{fresh_jsonl, write_jsonl, PARSE_COUNTER_LOCK};

    // The core memoization guarantee: a second call with an unchanged mtime
    // returns the cached derivation without re-reading/re-parsing the file.
    // Proven two ways at once — the parse counter must not advance, and a
    // surreptitious content rewrite that preserves the mtime must NOT leak
    // through (the cache trusts mtime, so it returns the stale-but-correct
    // value rather than re-deriving).
    #[test]
    fn derive_state_cached_skips_reparse_when_mtime_unchanged() {
        let _guard = PARSE_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = fresh_jsonl("skip-reparse");
        write_jsonl(
            &p,
            &[r#"{"type":"user","message":{"role":"user","content":"hello"}}"#],
        );

        let before = state_derive_parse_count();
        let first = derive_state_cached(&p).expect("first derive");
        assert_eq!(
            state_derive_parse_count(),
            before + 1,
            "first call is a cache miss → exactly one parse"
        );
        assert_eq!(first.state, SessionState::Processing);

        // Capture the mtime, rewrite the content to something that would derive
        // a *different* state, then restore the original mtime. The cache keys
        // on mtime, so it must return the original derivation untouched.
        let mtime = std::fs::metadata(&p).unwrap().modified().unwrap();
        write_jsonl(
            &p,
            &[
                r#"{"type":"assistant","message":{"role":"assistant","stop_reason":"end_turn","content":[]}}"#,
            ],
        );
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(mtime).expect("restore mtime");

        let second = derive_state_cached(&p).expect("second derive");
        assert_eq!(
            state_derive_parse_count(),
            before + 1,
            "second call with unchanged mtime must NOT re-parse"
        );
        assert_eq!(
            second.state,
            SessionState::Processing,
            "cache must return the original derivation, not re-read the rewritten file"
        );

        // A real mtime bump invalidates the entry and re-derives the new state.
        let later = mtime + std::time::Duration::from_secs(2);
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(later)
            .expect("bump mtime");
        let third = derive_state_cached(&p).expect("third derive");
        assert_eq!(
            state_derive_parse_count(),
            before + 2,
            "mtime change is a cache miss → one more parse"
        );
        assert_eq!(third.state, SessionState::WaitingForInput);

        let _ = std::fs::remove_file(&p);
    }

    // Once a first user message exists, the summary cache never re-reads the
    // head — even after the file's content (and mtime) change.
    #[test]
    fn first_user_message_cached_is_immutable_per_path() {
        // Serialized with every other test that touches the process-global
        // caches: retain_cached_evicts_unvisited_paths empties them mid-run,
        // which flakes any unguarded concurrent cache assertion.
        let _guard = PARSE_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = fresh_jsonl("summary-immutable");
        write_jsonl(
            &p,
            &[r#"{"type":"user","message":{"role":"user","content":"original first message"}}"#],
        );
        assert_eq!(
            first_user_message_cached(&p).as_deref(),
            Some("original first message")
        );

        // Rewrite with a different first message AND bump the mtime — the
        // summary cache is not mtime-keyed, so it must keep the original.
        write_jsonl(
            &p,
            &[
                r#"{"type":"user","message":{"role":"user","content":"a totally different message"}}"#,
            ],
        );
        assert_eq!(
            first_user_message_cached(&p).as_deref(),
            Some("original first message"),
            "immutable summary must not be re-read"
        );

        let _ = std::fs::remove_file(&p);
    }

    // A scan can observe a fresh session's JSONL after Claude Code flushes its
    // meta lines but before the first user prompt lands. That None must not be
    // cached, or the session stays summary-less (and Haiku-title-less) for the
    // life of the process.
    #[test]
    fn first_user_message_cached_does_not_cache_none() {
        let _guard = PARSE_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = fresh_jsonl("summary-none-not-cached");
        write_jsonl(
            &p,
            &[
                r#"{"type":"mode","mode":"normal","sessionId":"s"}"#,
                r#"{"type":"permission-mode","permissionMode":"default","sessionId":"s"}"#,
            ],
        );
        assert_eq!(
            first_user_message_cached(&p),
            None,
            "no user message yet → None"
        );

        // The first prompt arrives; the next scan tick must pick it up.
        write_jsonl(
            &p,
            &[
                r#"{"type":"mode","mode":"normal","sessionId":"s"}"#,
                r#"{"type":"permission-mode","permissionMode":"default","sessionId":"s"}"#,
                r#"{"type":"user","message":{"role":"user","content":"first real prompt"}}"#,
            ],
        );
        assert_eq!(
            first_user_message_cached(&p).as_deref(),
            Some("first real prompt"),
            "None must not have been cached"
        );

        let _ = std::fs::remove_file(&p);
    }

    // A first prompt that starts inside the 4KB head window but extends past
    // it is discarded as a partial line by read_jsonl_head — the larger retry
    // window must recover it.
    #[test]
    fn first_user_message_cached_finds_message_straddling_head_window() {
        let _guard = PARSE_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = fresh_jsonl("summary-straddle");
        let pad = "x".repeat(3500);
        let long_msg = format!("start of a long pasted prompt {}", "y".repeat(2000));
        write_jsonl(
            &p,
            &[
                &format!(r#"{{"type":"file-history-snapshot","snapshot":{{"pad":"{pad}"}}}}"#),
                &format!(r#"{{"type":"user","message":{{"role":"user","content":"{long_msg}"}}}}"#),
            ],
        );
        let got = first_user_message_cached(&p).expect("retry window must find the message");
        assert!(got.starts_with("start of a long pasted prompt"));

        let _ = std::fs::remove_file(&p);
    }

    // retain_cached evicts entries whose paths are absent from the visited set,
    // forcing a re-parse on the next access (proving the entry was dropped).
    #[test]
    fn retain_cached_evicts_unvisited_paths() {
        let _guard = PARSE_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = fresh_jsonl("evict");
        write_jsonl(
            &p,
            &[r#"{"type":"user","message":{"role":"user","content":"hi"}}"#],
        );
        let before = state_derive_parse_count();
        let _ = derive_state_cached(&p).expect("derive");
        assert_eq!(state_derive_parse_count(), before + 1);

        // Evict with an empty visited set (this path was not seen this scan).
        retain_cached(&HashSet::new());

        // Next access misses the cache → re-parses.
        let _ = derive_state_cached(&p).expect("derive after evict");
        assert_eq!(
            state_derive_parse_count(),
            before + 2,
            "evicted entry must be re-parsed on next access"
        );

        let _ = std::fs::remove_file(&p);
    }
}
