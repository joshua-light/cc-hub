use super::run::run_with_timeout;
use crate::config;
use log::{debug, warn};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

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

/// Ask the user's login shell once to resolve the configured spawn
/// command to its real argv. We only pay the `-ic` tax here; every actual
/// title generation then runs the resolved binary directly, avoiding both
/// the overhead of starting zsh and the tty fight an interactive shell
/// would cause.
///
/// Recognizes either a path (from `command -v`) or an alias body (from
/// `alias <name>`, whose output is roughly `<name>='claude …'` in zsh /
/// `alias <name>='claude …'` in bash).
/// The configured spawn command resolved to a direct argv (alias bodies
/// expanded), cached. `None` when the shell couldn't resolve it.
pub fn spawn_argv() -> Option<Vec<String>> {
    resolve_spawn_command()
}

pub(super) fn resolve_spawn_command() -> Option<Vec<String>> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
            parse_resolution(
                "EDITOR=nvim\nPAGER=less\nGREP_OPTIONS=--color\n",
                "cc-hub-new"
            ),
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
            parse_resolution(
                "claude --dangerously-skip-permissions @args\n",
                "cc-hub-new"
            ),
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
