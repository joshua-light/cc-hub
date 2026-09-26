//! Cheap 2-3 word titles for sessions, generated once via `cc-hub-new -p`
//! (Haiku) and cached forever on disk.
//!
//! Runs in a dedicated scratch cwd so the JSONL that Claude Code writes for
//! each `-p` invocation lands in a directory the scanner can filter out in
//! one comparison — otherwise every title generation would materialize as a
//! spurious "Inactive" session in the grid.
//!
//! - [`run`] — subprocess plumbing: deadline loop, tty detach, shutdown.
//! - [`resolve`] — resolves the spawn command through the login shell.

mod resolve;
mod run;

use crate::config;
use crate::platform::paths;
use log::warn;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub use resolve::spawn_argv;
pub(crate) use run::{detach_from_tty, shutting_down};
pub use run::{request_shutdown, run_claude_blocking, run_with_timeout};

#[derive(Default, Serialize, Deserialize)]
struct TitleCacheFile {
    titles: HashMap<String, String>,
}

/// Serializes concurrent writers so a load/insert/save cycle from one
/// titling task can't race another's. Scanners reading the file are
/// independently safe thanks to the tmp-and-rename in [`save`].
static WRITE_LOCK: Mutex<()> = Mutex::new(());

fn cache_file() -> PathBuf {
    paths::cache_dir().join("session-titles.json")
}

/// Scratch cwd used for every `cc-hub-new -p` run. Pinned so the scanner
/// can skip this directory in a single equality check and the Claude
/// projects dir contains at most one encoded folder for all our summaries.
///
/// Canonicalized at init: on macOS `/tmp` is a symlink to `/private/tmp`, so
/// the cwd Claude Code records in JSONL is the resolved form. Storing the
/// canonical path here keeps both the string compare in `is_scratch_cwd` and
/// the encoded-projects-dir skip in the scanner aligned with what's on disk.
pub fn scratch_cwd() -> &'static Path {
    static SCRATCH: OnceLock<PathBuf> = OnceLock::new();
    SCRATCH.get_or_init(|| {
        let base = PathBuf::from("/tmp/cc-hub-summaries");
        let _ = fs::create_dir_all(&base);
        fs::canonicalize(&base).unwrap_or(base)
    })
}

/// Current on-disk map of `session_id → title`. Empty on any read/parse
/// failure — a missing cache is the normal first-run state.
pub fn load() -> HashMap<String, String> {
    let path = cache_file();
    let Ok(data) = fs::read_to_string(&path) else {
        return HashMap::new();
    };
    match serde_json::from_str::<TitleCacheFile>(&data) {
        Ok(v) => v.titles,
        Err(e) => {
            warn!("title cache parse error at {}: {}", path.display(), e);
            HashMap::new()
        }
    }
}

fn save(titles: &HashMap<String, String>) -> std::io::Result<()> {
    let path = cache_file();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(&TitleCacheFile {
        titles: titles.clone(),
    })?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &path)
}

/// Atomically insert `title` under `sid`. Holds [`WRITE_LOCK`] across the
/// load/insert/save cycle so two concurrent titlers can't clobber each
/// other's entries.
pub fn persist_title(sid: &str, title: &str) -> std::io::Result<()> {
    let _g = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut map = load();
    map.insert(sid.to_string(), title.to_string());
    save(&map)
}

/// [`persist_title`], unless `sid` already has a name — the user's rename, or
/// the one it was born with — which is never overwritten. Returns whether
/// `title` was written.
pub fn name_if_nameless(sid: &str, title: &str) -> std::io::Result<bool> {
    let _g = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut map = load();
    if map.contains_key(sid) {
        return Ok(false);
    }
    map.insert(sid.to_string(), title.to_string());
    save(&map).map(|()| true)
}

/// Generate a sanitized short title for a session by running the title
/// prompt through `run_claude_blocking`. `None` when titling is disabled,
/// the input is empty, or the underlying Claude call fails.
pub fn generate_title_blocking(first_msg: &str) -> Option<String> {
    let title_cfg = &config::get().title;
    if !title_cfg.enabled {
        return None;
    }
    if first_msg.trim().is_empty() {
        return None;
    }
    let prompt = format!("{}{}", title_cfg.prompt, first_msg);
    let raw = run_claude_blocking(&title_cfg.model, &prompt, title_cfg.run_timeout())?;
    sanitize_title(&raw, title_cfg.max_length)
}

fn sanitize_title(raw: &str, max: usize) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let cleaned: String = line
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '.' || c == '`' || c.is_whitespace())
        .to_string();
    if cleaned.is_empty() {
        return None;
    }
    let mut end = cleaned.len().min(max);
    while end > 0 && !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    Some(cleaned[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn naming_never_overwrites_a_name() {
        crate::test_util::with_temp_home(|| {
            assert!(name_if_nameless("sid-1", "Task: born with").unwrap());
            persist_title("sid-1", "renamed by hand").unwrap();
            assert!(!name_if_nameless("sid-1", "Task: born with").unwrap());
            assert_eq!(load()["sid-1"], "renamed by hand");
        });
    }

    #[test]
    fn sanitize_strips_quotes_and_trailing_period() {
        assert_eq!(
            sanitize_title("\"refactor auth module\"", 40),
            Some("refactor auth module".into())
        );
        assert_eq!(
            sanitize_title("Fix flaky test.", 40),
            Some("Fix flaky test".into())
        );
    }

    #[test]
    fn sanitize_takes_first_nonempty_line() {
        assert_eq!(
            sanitize_title("\n\n  Debug CI  \nignore this", 40),
            Some("Debug CI".into())
        );
    }

    #[test]
    fn sanitize_empty_returns_none() {
        assert_eq!(sanitize_title("", 40), None);
        assert_eq!(sanitize_title("   \n", 40), None);
    }

    #[test]
    fn sanitize_clamps_long_output() {
        let long = "a".repeat(100);
        let out = sanitize_title(&long, 40).unwrap();
        assert!(out.len() <= 40);
    }

    #[test]
    fn sanitize_respects_custom_max_length() {
        let long = "a".repeat(100);
        let out = sanitize_title(&long, 10).unwrap();
        assert_eq!(out.len(), 10);
    }

    #[test]
    fn generate_title_blocking_rejects_empty_input() {
        assert_eq!(generate_title_blocking("   \n"), None);
    }
}
