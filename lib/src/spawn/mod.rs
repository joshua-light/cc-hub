//! Spawn agent sessions backed by detached multiplexer sessions.
//!
//! Detachment lets the hub inject prompts via `send-keys` without stealing
//! focus, and the agent survives an accidentally-closed terminal. Users
//! attach on demand via the hub UI.
//!
//! - [`command`]: builds the agent's shell command line.
//! - [`trust`]: marks the cwd trusted in Claude's `.claude.json`.

use crate::agent::{AgentConfig, AgentKind};
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
use trust::{ensure_path_trusted, ensure_path_trusted_at};

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
    spawn_agent_session_with_config(&agent, cwd, target, initial_prompt, model, readonly_tools)
}

pub fn spawn_claude_session(cwd: &str, resume_id: Option<&str>) -> io::Result<String> {
    spawn_agent_session(
        "claude",
        cwd,
        resume_id.map(|sid| SessionTarget::Resume(sid.to_string())),
        None,
        None,
        false,
    )
}

pub fn spawn_agent_session_with_config(
    agent: &AgentConfig,
    cwd: &str,
    target: Option<SessionTarget>,
    initial_prompt: Option<&str>,
    model: Option<&str>,
    readonly_tools: bool,
) -> io::Result<String> {
    let name = unique_session_name("cchub");
    let cmd = build_agent_command(
        agent,
        cwd,
        &name,
        target,
        initial_prompt,
        model,
        readonly_tools,
    )?;
    if agent.kind == AgentKind::Claude {
        if let Some(account) = crate::resources::for_agent(&agent.id) {
            let path = if account.home_mode.as_deref() == Some("default") {
                dirs::home_dir().map(|h| h.join(".claude.json"))
            } else {
                account.home().map(|h| h.join(".claude.json"))
            };
            if let Some(path) = path {
                ensure_path_trusted_at(cwd, path)?;
            }
        } else {
            ensure_path_trusted(cwd)?;
        }
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

pub fn spawn_shell_tmux_session(cwd: &str) -> io::Result<String> {
    let name = unique_session_name("cchub-sh");
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
    use super::stalled_spawn_message;

    // The real failure this was built for: a pane blocked at oh-my-zsh's
    // update prompt. The prompt line must surface in the status message so
    // the user can see *why* the agent never appeared.
    #[test]
    fn stalled_message_surfaces_last_pane_line() {
        let pane = "\n[oh-my-zsh] Would you like to update? [Y/n]\n\n";
        assert_eq!(
            stalled_spawn_message("claude", "cchub-1-2", pane),
            "claude stuck in [cchub-1-2]: [oh-my-zsh] Would you like to update? [Y/n]"
        );
    }

    #[test]
    fn stalled_message_handles_blank_pane() {
        assert_eq!(
            stalled_spawn_message("claude", "cchub-1-2", "\n\n"),
            "claude not started in [cchub-1-2] (blank pane)"
        );
    }
}
