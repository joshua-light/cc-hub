//! Cheap 2-3 word titles for sessions, generated once via `cc-hub-new -p`
//! (Haiku) and cached forever on disk.
//!
//! Runs in a dedicated scratch cwd so the JSONL that Claude Code writes for
//! each `-p` invocation lands in a directory the scanner can filter out in
//! one comparison — otherwise every title generation would materialize as a
//! spurious "Inactive" session in the grid.

use crate::config;
use crate::platform::paths;
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Signal all in-flight title subprocesses to kill their children and
/// return, so quitting the app doesn't block on up to ~45s of pending
/// Haiku calls. Call this once from the TUI just before cleanup.
pub fn request_shutdown() {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

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

/// Cached result of resolving the configured spawn command through the
/// user's login shell. `Some(argv)` is the direct argv to exec, skipping
/// the shell on every call; `None` means the last resolve attempt failed
/// (e.g., transient shell hiccup, missing alias). The cache TTLs out so a
/// single failure can't permanently disable titling for the process.
static RESOLVED_CMD: Mutex<Option<ResolveCache>> = Mutex::new(None);

struct ResolveCache {
    fetched_at: Instant,
    value: Option<Vec<String>>,
}

/// Put the child in its own session so it can't touch our controlling
/// terminal. An interactive zsh left in our session calls `tcsetpgrp` on
/// `/dev/tty` as part of its job-control setup — with the TUI owning that
/// same tty, the parent's raw-mode / alt-screen state ends up scrambled.
/// `setsid` both gives the child a fresh process group and detaches it
/// from any controlling terminal; a later `open("/dev/tty")` then fails
/// cleanly instead of hijacking ours.
#[cfg(unix)]
fn detach_from_tty(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn detach_from_tty(_cmd: &mut Command) {}

/// Spawn `cmd` and poll `try_wait` until it finishes or `timeout` expires,
/// killing on timeout. Stdin/stdout/stderr configuration is the caller's
/// responsibility — this helper just owns the deadline loop so resolution
/// and generation don't duplicate it.
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Option<Output> {
    detach_from_tty(&mut cmd);
    let mut child: Child = cmd
        .spawn()
        .map_err(|e| warn!("title: spawn failed: {}", e))
        .ok()?;

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if SHUTDOWN.load(Ordering::Relaxed) {
                    debug!("title: shutdown signal, killing subprocess");
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                if std::time::Instant::now() >= deadline {
                    warn!("title: subprocess timed out after {:?}, killing", timeout);
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                // Short poll so a quit that lands mid-sleep adds at most
                // 100ms of quit latency per in-flight title.
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                warn!("title: try_wait failed: {}", e);
                return None;
            }
        }
    }
    child.wait_with_output().ok()
}

/// Ask the user's login shell once to resolve the configured spawn
/// command to its real argv. We only pay the `-ic` tax here; every actual
/// title generation then runs the resolved binary directly, avoiding both
/// the overhead of starting zsh and the tty fight an interactive shell
/// would cause.
///
/// Recognizes either a path (from `command -v`) or an alias body (from
/// `alias <name>`, whose output is roughly `<name>='claude …'` in zsh /
/// `alias <name>='claude …'` in bash).
fn resolve_spawn_command() -> Option<Vec<String>> {
    // Successful resolutions are stable enough to cache for an hour; failures
    // re-attempt every minute so a transient shell hiccup doesn't disable
    // titling for the rest of the process.
    const SUCCESS_TTL: Duration = Duration::from_secs(3600);
    const FAILURE_TTL: Duration = Duration::from_secs(60);

    let mut guard = RESOLVED_CMD.lock().unwrap_or_else(|e| e.into_inner());
    let fresh = guard.as_ref().is_some_and(|c| {
        let ttl = if c.value.is_some() {
            SUCCESS_TTL
        } else {
            FAILURE_TTL
        };
        c.fetched_at.elapsed() < ttl
    });
    if !fresh {
        let value = compute_resolve();
        *guard = Some(ResolveCache {
            fetched_at: Instant::now(),
            value,
        });
    }
    guard.as_ref().and_then(|c| c.value.clone())
}

fn compute_resolve() -> Option<Vec<String>> {
    let cmd_name = &config::get().spawn.command;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
    // `command -v` is POSIX and prints the path for a binary; if it isn't
    // found we fall through to `alias`, which prints the body so we can
    // recover an alias expansion. Redirecting stderr to /dev/null keeps
    // shell chatter out of our stdout parse.
    let script = format!(
        "command -v {name} 2>/dev/null; alias {name} 2>/dev/null",
        name = cmd_name
    );
    let mut cmd = Command::new(&shell);
    cmd.arg("-ic")
        .arg(&script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let output = run_with_timeout(cmd, config::get().title.resolve_timeout())?;
    if !output.status.success() {
        warn!("title: alias resolve exit={}", output.status);
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let argv = parse_resolution(&raw);
    match &argv {
        Some(v) => debug!("title: resolved {} → {:?}", cmd_name, v),
        None => warn!(
            "title: could not resolve {} from shell output: {:?}",
            cmd_name,
            raw.trim()
        ),
    }
    argv
}

/// Parse the output of `command -v <name>; alias <name>`. Prefers a full
/// path on the first line; otherwise looks for an alias body between the
/// first `=` and the trailing newline, stripping surrounding single or
/// double quotes. Returns `None` if no usable line is present.
fn parse_resolution(raw: &str) -> Option<Vec<String>> {
    for line in raw.lines().map(str::trim).filter(|l| !l.is_empty()) {
        // `command -v` emits an absolute path we can exec as-is.
        if line.starts_with('/') {
            return Some(vec![line.to_string()]);
        }
        // `alias cc-hub-new` emits `cc-hub-new='claude …'` (zsh) or
        // `alias cc-hub-new='claude …'` (bash). Both have one `=`
        // separating the name from a quoted body.
        if let Some(eq) = line.find('=') {
            let body = line[eq + 1..].trim();
            let body = body
                .strip_prefix('\'')
                .and_then(|s| s.strip_suffix('\''))
                .or_else(|| body.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
                .unwrap_or(body);
            let argv: Vec<String> = body.split_whitespace().map(str::to_string).collect();
            if !argv.is_empty() {
                return Some(argv);
            }
        }
    }
    None
}

/// Run `<spawn.command> --model <model> -p <prompt>` in the scratch cwd
/// and return the raw stdout. Resolves the configured spawn command
/// through the user's login shell on first call (cached afterwards), then
/// execs the resolved binary directly. Returns `None` on any failure
/// (resolve, spawn, non-zero exit, timeout, shutdown).
pub fn run_claude_blocking(model: &str, prompt: &str, timeout: Duration) -> Option<String> {
    fs::create_dir_all(scratch_cwd()).ok()?;
    let resolved = resolve_spawn_command()?;
    let (exe, base_args) = resolved.split_first()?;
    let mut cmd = Command::new(exe);
    cmd.args(base_args)
        .arg("--model")
        .arg(model)
        .arg("-p")
        .arg(prompt)
        .current_dir(scratch_cwd())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    debug!(
        "claude_blocking: model={} prompt_len={} timeout={:?}",
        model,
        prompt.len(),
        timeout
    );

    let output = run_with_timeout(cmd, timeout)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            "claude_blocking: {} exit={} stderr={:?}",
            config::get().spawn.command,
            output.status,
            stderr.trim()
        );
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
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

    #[test]
    fn parse_resolution_prefers_absolute_path() {
        assert_eq!(
            parse_resolution("/usr/local/bin/cc-hub-new\n"),
            Some(vec!["/usr/local/bin/cc-hub-new".into()])
        );
    }

    #[test]
    fn parse_resolution_zsh_alias() {
        // `alias cc-hub-new` in zsh prints `cc-hub-new='claude --flag'`.
        assert_eq!(
            parse_resolution("cc-hub-new='claude --dangerously-skip-permissions'\n"),
            Some(vec![
                "claude".into(),
                "--dangerously-skip-permissions".into()
            ])
        );
    }

    #[test]
    fn parse_resolution_bash_alias() {
        // `alias cc-hub-new` in bash prints `alias cc-hub-new='claude …'`.
        assert_eq!(
            parse_resolution("alias cc-hub-new='claude --model haiku'\n"),
            Some(vec!["claude".into(), "--model".into(), "haiku".into()])
        );
    }

    #[test]
    fn parse_resolution_path_wins_over_alias() {
        // Both lines present: pick the path, skip the alias.
        assert_eq!(
            parse_resolution("/opt/bin/cc-hub-new\ncc-hub-new='claude'\n"),
            Some(vec!["/opt/bin/cc-hub-new".into()])
        );
    }

    #[test]
    fn parse_resolution_empty_returns_none() {
        assert_eq!(parse_resolution(""), None);
        assert_eq!(parse_resolution("\n\n  \n"), None);
    }
}
