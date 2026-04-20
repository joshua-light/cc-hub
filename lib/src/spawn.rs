//! Spawn a new Claude session backed by a detached multiplexer session.
//!
//! Detachment lets the hub inject prompts via `send-keys` without stealing
//! focus, and the agent survives an accidentally-closed terminal. Users
//! attach on demand via the hub UI. The command run in the pane is
//! [`config::SpawnConfig::command`] — `cc-hub-new` by default.

use crate::config;
use crate::platform::mux;
#[cfg(not(windows))]
use crate::platform::{paths, terminal};
#[cfg(not(windows))]
use log::info;
use std::io;
#[cfg(not(windows))]
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// Spawn a new Claude session for `cwd`, returning the multiplexer session
/// name. When `resume_id` is `Some`, the pane runs `<cmd> --resume <id>`
/// instead of a fresh `<cmd>`.
pub fn spawn_claude_session(cwd: &str, resume_id: Option<&str>) -> io::Result<String> {
    let name = unique_session_name("cchub");
    let base = config::get().spawn.command.as_str();
    let cmd = match resume_id {
        Some(sid) => format!("{} --resume {}", base, sid),
        None => base.to_string(),
    };
    mux::spawn_detached(&name, cwd, Some(&cmd))?;
    Ok(name)
}

/// Spawn an ephemeral multiplexer session that runs an interactive shell in
/// `cwd`. Pair with [`crate::tmux_pane::TmuxPaneView::spawn_owned`] so closing
/// the popup tears the session down.
pub fn spawn_shell_tmux_session(cwd: &str) -> io::Result<String> {
    let name = unique_session_name("cchub-sh");
    mux::spawn_detached(&name, cwd, None)?;
    Ok(name)
}

/// Open a new terminal window that attaches to an existing detached session.
/// Used to recover a session whose terminal was closed — the agent kept
/// running headlessly, this brings it back into view.
///
/// Unix-only today. Windows embeds sessions inside the hub via
/// [`crate::tmux_pane`]; there's no separate terminal window to resurrect.
#[cfg(not(windows))]
pub fn attach_tmux_session(tmux_name: &str, cwd: &str) -> io::Result<String> {
    let launcher = terminal::pick().ok_or_else(|| {
        io::Error::other("no terminal emulator found (set $TERMINAL or install kitty/foot/alacritty)")
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
    // Windows path uses the in-TUI embed (tmux_pane) exclusively.
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
