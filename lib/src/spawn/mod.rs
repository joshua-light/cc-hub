//! Spawn agent sessions backed by detached multiplexer sessions.
//!
//! Detachment lets the hub inject prompts via `send-keys` without stealing
//! focus, and the agent survives an accidentally-closed terminal. Users
//! attach on demand via the hub UI.
//!
//! - `command`: builds the agent's shell command line.
//! - `trust`: marks the cwd trusted in Claude's `.claude.json`.

use crate::agent::AgentKind;
use crate::config;
use crate::platform::mux;
#[cfg(not(windows))]
use crate::platform::paths;
#[cfg(not(windows))]
use crate::platform::terminal;
use command::build_agent_command;
#[cfg(not(windows))]
use log::info;
use std::io;
use std::path::PathBuf;
#[cfg(not(windows))]
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

mod command;
mod trust;

#[derive(Clone, Debug)]
/// Which session a spawned agent process is: an existing one to continue, or
/// a new one the hub has already given an id.
pub enum SessionTarget {
    /// Continue an existing session by id (`claude --resume`, `codex resume`,
    /// `pi --session`).
    Resume(String),
    /// Continue an existing session by transcript path (Pi only).
    ResumeFile(PathBuf),
    /// Start a new session under an id chosen up front (`claude --session-id`),
    /// so the hub can attach state — a title — before the agent even starts.
    Fresh(String),
}

pub fn spawn_agent_session(
    agent_id: &str,
    cwd: &str,
    target: Option<SessionTarget>,
    initial_prompt: Option<&str>,
    model: Option<&str>,
    readonly_tools: bool,
) -> io::Result<String> {
    let agent = config::get()
        .agent(agent_id)
        .ok_or_else(|| io::Error::other(format!("unknown agent id: {}", agent_id)))?;
    let name = unique_session_name("hal");
    let cmd = build_agent_command(&agent, &name, target, initial_prompt, model, readonly_tools)?;
    if agent.kind == AgentKind::Claude {
        trust::ensure_trusted_for(&agent.id, cwd)?;
    }
    mux::spawn_detached(&name, cwd, Some(&cmd))?;
    Ok(name)
}

/// Post-mortem for a spawn watchdog that expired: the detached session either
/// exited during startup (rc error, missing alias) or is stuck before the
/// agent ran — e.g. a shell-rc prompt waiting for input nobody can type into
/// a detached pane. Returns a one-line status for the bar; the full pane
/// content goes to the log.
pub fn diagnose_stalled_spawn(tmux_name: &str, agent: &str) -> String {
    if !mux::has_session(tmux_name) {
        return format!("{} [{}] exited during startup", agent, tmux_name);
    }
    let pane = mux::capture_pane(tmux_name);
    log::warn!(
        "stalled spawn [{}] pane content:\n{}",
        tmux_name,
        pane.trim_end()
    );
    stalled_spawn_message(agent, tmux_name, &pane)
}

/// Pure message builder for [`diagnose_stalled_spawn`]. The last non-empty
/// pane line is the best available hint — for a blocked rc prompt it is the
/// prompt itself (e.g. "[oh-my-zsh] Would you like to update? [Y/n]").
fn stalled_spawn_message(agent: &str, tmux_name: &str, pane: &str) -> String {
    match pane.lines().rev().find(|l| !l.trim().is_empty()) {
        Some(line) => format!("{} stuck in [{}]: {}", agent, tmux_name, line.trim()),
        None => format!("{} not started in [{}] (blank pane)", agent, tmux_name),
    }
}

/// The default launch command's name before cc-hub became hal. Users who set
/// up the old alias and upgraded have only this one defined.
const LEGACY_LAUNCH_COMMAND: &str = "cc-hub-new";

/// What the user's shell knows about an agent's launch command.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(windows, allow(dead_code))] // Windows never probes; see below.
enum LaunchProbe {
    Defined,
    /// Undefined; `legacy` when the pre-rename `cc-hub-new` still is.
    Missing {
        legacy: bool,
    },
    /// The probe itself failed (timeout, no shell), so no verdict.
    Unknown,
}

/// Why `agent`'s detached session ended before the agent came up, for the
/// status bar. The common cause is a launch command the shell can't find —
/// a missing alias, or one still named for cc-hub — so this asks the user's
/// interactive shell. Blocks for one shell startup; call it off the UI
/// thread.
pub fn explain_exited_spawn(agent: &str) -> String {
    let command = config::get()
        .agent(agent)
        .map(|a| a.command)
        .unwrap_or_default();
    let Some(name) = command.split_whitespace().next() else {
        return format!("{agent} exited during startup");
    };
    exited_spawn_message(agent, name, &shell_name(), probe_launch_command(name))
}

fn exited_spawn_message(agent: &str, name: &str, shell: &str, probe: LaunchProbe) -> String {
    match probe {
        LaunchProbe::Missing { legacy: true } => format!(
            "{agent} didn't start: {name} isn't defined in {shell}. \
             cc-hub is now hal: rename your {LEGACY_LAUNCH_COMMAND} alias to {name}"
        ),
        LaunchProbe::Missing { legacy: false } if name == config::DEFAULT_LAUNCH_COMMAND => {
            format!(
                "{agent} didn't start: {name} isn't defined in {shell}. \
             Add alias {name}='claude' to your shell rc"
            )
        }
        LaunchProbe::Missing { legacy: false } => format!(
            "{agent} didn't start: {name} isn't defined in {shell}. \
             Install it or fix [agents.{agent}].command"
        ),
        LaunchProbe::Defined | LaunchProbe::Unknown => {
            format!("{agent} exited during startup. Run {name} in a terminal to see why")
        }
    }
}

#[cfg(not(windows))]
fn shell_name() -> String {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
    std::path::Path::new(&shell)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or(shell)
}

#[cfg(windows)]
fn shell_name() -> String {
    "PowerShell".into()
}

/// Ask the shell the agent launches through whether `name` resolves. Same
/// `-ic` and update kill-switch as the spawn itself, so rc aliases count and
/// no updater prompt blocks the probe. The markers are echoed rather than
/// read from the exit code: rc chatter can't fake them, and a failing rc
/// line can't mask them.
#[cfg(not(windows))]
fn probe_launch_command(name: &str) -> LaunchProbe {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
    let defined = |cmd: &str, marker: &str| {
        format!(
            "command -v {} >/dev/null 2>&1 && echo {marker};",
            terminal::shell_quote(cmd)
        )
    };
    let mut script = defined(name, "__hal_defined__");
    if name == config::DEFAULT_LAUNCH_COMMAND {
        script.push_str(&defined(LEGACY_LAUNCH_COMMAND, "__hal_legacy__"));
    }
    let mut cmd = Command::new(shell);
    cmd.env("DISABLE_AUTO_UPDATE", "true")
        .arg("-ic")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let Some(output) = crate::title::run_with_timeout(cmd, config::get().title.resolve_timeout())
    else {
        return LaunchProbe::Unknown;
    };
    let out = String::from_utf8_lossy(&output.stdout);
    let has = |marker: &str| out.lines().any(|l| l.trim() == marker);
    if has("__hal_defined__") {
        LaunchProbe::Defined
    } else {
        LaunchProbe::Missing {
            legacy: has("__hal_legacy__"),
        }
    }
}

/// A failed launch leaves pwsh running in the pane on Windows, so the stalled
/// path's pane capture reports it; there is nothing more to probe here.
#[cfg(windows)]
fn probe_launch_command(_name: &str) -> LaunchProbe {
    LaunchProbe::Unknown
}

/// Start a terminal — a plain shell in `cwd`, in a detached session the
/// terminal scan ([`crate::sessions::terminal`]) knows by its name.
pub fn spawn_terminal(cwd: &str) -> io::Result<String> {
    let name = unique_session_name(crate::sessions::terminal::TMUX_PREFIX);
    mux::spawn_detached(&name, cwd, None)?;
    Ok(name)
}

#[cfg(not(windows))]
pub fn attach_tmux_session(tmux_name: &str, cwd: &str) -> io::Result<String> {
    let launcher = terminal::pick().ok_or_else(|| {
        io::Error::other(
            "no terminal emulator found (set $TERMINAL or install kitty/foot/alacritty)",
        )
    })?;
    let attach_argv = mux::attach_argv(tmux_name);
    let attach_argv_refs: Vec<&str> = attach_argv.iter().map(|s| s.as_str()).collect();
    let argv = launcher.argv_bare(cwd, &attach_argv_refs);
    let bin = paths::terminal_wrapper_script(launcher.name())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| launcher.name().to_string());

    info!("attach: {} {}", bin, argv.join(" "));
    Command::new(&bin)
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(format!("reattached {} in {}", tmux_name, launcher.name()))
}

#[cfg(windows)]
pub fn attach_tmux_session(_tmux_name: &str, _cwd: &str) -> io::Result<String> {
    Err(io::Error::other(
        "attach_tmux_session: not implemented on Windows (embed via TmuxPaneView instead)",
    ))
}

fn unique_session_name(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}-{}", prefix, std::process::id(), nanos)
}

#[cfg(test)]
mod tests {
    use super::{exited_spawn_message, stalled_spawn_message, LaunchProbe};

    // The upgrade case: the shell still has the alias under its cc-hub name.
    #[test]
    fn a_missing_command_with_the_old_alias_says_to_rename_it() {
        assert_eq!(
            exited_spawn_message(
                "claude",
                "hal-new",
                "zsh",
                LaunchProbe::Missing { legacy: true }
            ),
            "claude didn't start: hal-new isn't defined in zsh. \
             cc-hub is now hal: rename your cc-hub-new alias to hal-new"
        );
    }

    #[test]
    fn a_missing_default_command_says_how_to_define_it() {
        assert_eq!(
            exited_spawn_message(
                "claude",
                "hal-new",
                "bash",
                LaunchProbe::Missing { legacy: false }
            ),
            "claude didn't start: hal-new isn't defined in bash. \
             Add alias hal-new='claude' to your shell rc"
        );
    }

    #[test]
    fn a_missing_custom_command_points_at_the_agent_config() {
        assert_eq!(
            exited_spawn_message(
                "codex",
                "codex",
                "zsh",
                LaunchProbe::Missing { legacy: false }
            ),
            "codex didn't start: codex isn't defined in zsh. \
             Install it or fix [agents.codex].command"
        );
    }

    #[test]
    fn a_defined_command_that_exited_says_to_run_it_by_hand() {
        for probe in [LaunchProbe::Defined, LaunchProbe::Unknown] {
            assert_eq!(
                exited_spawn_message("claude", "hal-new", "zsh", probe),
                "claude exited during startup. Run hal-new in a terminal to see why"
            );
        }
    }

    // A pane blocked at oh-my-zsh's update prompt: the prompt line must
    // surface in the status message so the user can see *why* the agent
    // never appeared.
    #[test]
    fn stalled_message_surfaces_last_pane_line() {
        let pane = "\n[oh-my-zsh] Would you like to update? [Y/n]\n\n";
        assert_eq!(
            stalled_spawn_message("claude", "hal-1-2", pane),
            "claude stuck in [hal-1-2]: [oh-my-zsh] Would you like to update? [Y/n]"
        );
    }

    #[test]
    fn stalled_message_handles_blank_pane() {
        assert_eq!(
            stalled_spawn_message("claude", "hal-1-2", "\n\n"),
            "claude not started in [hal-1-2] (blank pane)"
        );
    }
}
