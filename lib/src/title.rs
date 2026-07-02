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
    let mut cmd = resolve_command(cmd_name);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let output = run_with_timeout(cmd, config::get().title.resolve_timeout())?;
    if !output.status.success() {
        warn!("title: alias resolve exit={}", output.status);
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let argv = parse_resolution(&raw, cmd_name);
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

/// Build the subprocess whose stdout reveals how the configured spawn command
/// `name` resolves. The output is fed to [`parse_resolution`].
///
/// - POSIX: ask the user's login shell. `command -v` prints a binary's path;
///   we fall through to `alias`, which prints the body so an alias expansion
///   can be recovered. `stderr` is dropped so shell chatter stays out of the
///   stdout parse.
/// - Windows: the spawn command is usually a function defined in the user's
///   PowerShell `$PROFILE` (e.g. `cc-hub-new`), invisible to `command -v` and
///   to a non-profile shell. So we let `powershell.exe` load the profile and
///   print `(Get-Command name).Definition` — an application resolves to its
///   exe path, a function/alias to its body (e.g. `claude --flag @args`).
#[cfg(not(windows))]
fn resolve_command(name: &str) -> Command {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
    let script = format!("command -v {name} 2>/dev/null; alias {name} 2>/dev/null");
    let mut cmd = Command::new(shell);
    cmd.arg("-ic").arg(script);
    cmd
}

#[cfg(windows)]
fn resolve_command(name: &str) -> Command {
    let script = format!(
        "$c = Get-Command {name} -ErrorAction SilentlyContinue; if ($c) {{ $c.Definition }}"
    );
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoLogo", "-NonInteractive", "-Command", &script]);
    cmd
}

/// Parse the output of `command -v <cmd>; alias <cmd>` (or PowerShell's
/// `(Get-Command <cmd>).Definition`) into the argv to exec.
///
/// Only lines that actually resolve `cmd` are accepted: a path whose basename
/// is `cmd`, or an alias line of the form `<cmd>=…` / `alias <cmd>=…`. This is
/// deliberate — a `zsh -ic` runs the user's rc files first, so startup chatter
/// like `EDITOR=nvim` precedes the real output. The old "first line with `=`"
/// rule parsed that chatter as the alias body and cached the wrong binary for
/// an hour. Returns `None` when nothing resolves `cmd`, so the caller falls
/// back to running through the shell.
fn parse_resolution(raw: &str, cmd: &str) -> Option<Vec<String>> {
    for line in raw.lines().map(str::trim).filter(|l| !l.is_empty()) {
        // `command -v` (POSIX) emits an absolute path; PowerShell's
        // `(Get-Command exe).Definition` emits a drive-letter `…\foo.exe`
        // path. Accept it only when its basename is the command we queried —
        // rc-file chatter can print unrelated absolute paths we must not exec.
        // Matched before the splits below since a Windows path may contain
        // spaces.
        if (line.starts_with('/') || is_windows_exe_path(line)) && path_resolves_cmd(line, cmd) {
            return Some(vec![line.to_string()]);
        }
        // `alias <cmd>` emits `<cmd>='claude …'` (zsh) or
        // `alias <cmd>='claude …'` (bash). Only the line that defines THIS
        // command is the resolution; a stray `EDITOR=nvim` is ignored.
        if let Some(body) = alias_body_for(line, cmd) {
            let body = body.trim();
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
        // Windows: a PowerShell function/alias `.Definition` is its body, e.g.
        // `claude --dangerously-skip-permissions @args`. Split it and drop the
        // splat tokens; the head resolves against PATH at exec time.
        #[cfg(windows)]
        {
            let argv: Vec<String> = line
                .split_whitespace()
                .filter(|t| !t.eq_ignore_ascii_case("@args") && !t.eq_ignore_ascii_case("$args"))
                .map(str::to_string)
                .collect();
            if !argv.is_empty() {
                return Some(argv);
            }
        }
    }
    None
}

/// True when `line` (a POSIX absolute path or Windows `…\foo.exe`) names the
/// command we asked `command -v` / `Get-Command` about: its final path
/// component equals `cmd`, ignoring a trailing `.exe`. Guards against exec'ing
/// an unrelated absolute path printed by rc-file startup chatter.
fn path_resolves_cmd(line: &str, cmd: &str) -> bool {
    let base = line.rsplit(['/', '\\']).next().unwrap_or(line);
    let base = base
        .strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".EXE"))
        .unwrap_or(base);
    base.eq_ignore_ascii_case(cmd)
}

/// Extract the alias body for `cmd` from one line of `alias <cmd>` output.
/// Matches `<cmd>=<body>` (zsh) and `alias <cmd>=<body>` (bash), requiring the
/// `=` to sit immediately after the command name so a foreign assignment like
/// `EDITOR=nvim` (or a var whose name merely shares a prefix) never matches.
/// Returns the raw text after `=`; the caller trims and unquotes it.
fn alias_body_for<'a>(line: &'a str, cmd: &str) -> Option<&'a str> {
    let rest = line.strip_prefix("alias ").unwrap_or(line);
    rest.strip_prefix(cmd)?.strip_prefix('=')
}

/// True when `s` looks like an absolute Windows path to an `.exe` (drive-letter
/// root such as `C:\…\foo.exe`). Lets [`parse_resolution`] exec the path
/// verbatim instead of splitting it on any embedded spaces.
fn is_windows_exe_path(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() > 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
        && s.to_ascii_lowercase().ends_with(".exe")
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
            parse_resolution("/usr/local/bin/cc-hub-new\n", "cc-hub-new"),
            Some(vec!["/usr/local/bin/cc-hub-new".into()])
        );
    }

    #[test]
    fn parse_resolution_zsh_alias() {
        // `alias cc-hub-new` in zsh prints `cc-hub-new='claude --flag'`.
        assert_eq!(
            parse_resolution(
                "cc-hub-new='claude --dangerously-skip-permissions'\n",
                "cc-hub-new"
            ),
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
            parse_resolution("alias cc-hub-new='claude --model haiku'\n", "cc-hub-new"),
            Some(vec!["claude".into(), "--model".into(), "haiku".into()])
        );
    }

    #[test]
    fn parse_resolution_path_wins_over_alias() {
        // Both lines present: pick the path, skip the alias.
        assert_eq!(
            parse_resolution("/opt/bin/cc-hub-new\ncc-hub-new='claude'\n", "cc-hub-new"),
            Some(vec!["/opt/bin/cc-hub-new".into()])
        );
    }

    #[test]
    fn parse_resolution_ignores_rc_chatter_before_alias() {
        // `zsh -ic` sources rc files first, so assignments print before the
        // real alias output. A stray `EDITOR=nvim` must NOT be taken as the
        // resolution — only the line that actually defines cc-hub-new is.
        let raw = "EDITOR=nvim\nLESS=-R\ncc-hub-new='claude --flag'\n";
        assert_eq!(
            parse_resolution(raw, "cc-hub-new"),
            Some(vec!["claude".into(), "--flag".into()])
        );
    }

    #[test]
    fn parse_resolution_ignores_unrelated_path_chatter() {
        // An unrelated absolute path from startup chatter is not the
        // resolution; the alias line for the queried command is.
        let raw = "/opt/tools/some-other-bin\ncc-hub-new='claude'\n";
        assert_eq!(
            parse_resolution(raw, "cc-hub-new"),
            Some(vec!["claude".into()])
        );
    }

    #[test]
    fn parse_resolution_rejects_foreign_assignments_only() {
        // Nothing resolves the queried command → None, so the caller falls
        // back to spawning through the shell rather than exec'ing garbage.
        assert_eq!(
            parse_resolution("EDITOR=nvim\nPAGER=less\nGREP_OPTIONS=--color\n", "cc-hub-new"),
            None
        );
    }

    #[test]
    fn parse_resolution_rejects_prefix_named_assignment() {
        // `cc-hub-newer=x` shares a prefix with the command but is a different
        // var — the `=` must sit immediately after the exact command name.
        assert_eq!(
            parse_resolution("cc-hub-newer='wrong'\n", "cc-hub-new"),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn parse_resolution_windows_function_body() {
        // PowerShell `(Get-Command cc-hub-new).Definition` for a $PROFILE
        // function prints its body; split it and drop the `@args` splat.
        assert_eq!(
            parse_resolution("claude --dangerously-skip-permissions @args\n", "cc-hub-new"),
            Some(vec![
                "claude".into(),
                "--dangerously-skip-permissions".into()
            ])
        );
    }

    #[cfg(windows)]
    #[test]
    fn parse_resolution_windows_application_path() {
        // An application resolves to its exe path, exec'd verbatim. Its
        // basename matches the queried command (`claude` → `claude.exe`).
        assert_eq!(
            parse_resolution("C:\\Users\\me\\.local\\bin\\claude.exe\n", "claude"),
            Some(vec!["C:\\Users\\me\\.local\\bin\\claude.exe".into()])
        );
    }

    #[test]
    fn parse_resolution_empty_returns_none() {
        assert_eq!(parse_resolution("", "cc-hub-new"), None);
        assert_eq!(parse_resolution("\n\n  \n", "cc-hub-new"), None);
    }
}
