//! Spawn a new `ccyo` session backed by a detached tmux session.
//!
//! Detachment lets the hub inject prompts via `tmux send-keys` without
//! stealing focus, and the agent survives an accidentally-closed terminal.
//! Users attach on demand via the hub UI.

use crate::platform::{paths, terminal};
use crate::send;
use log::{error, info};
use std::io;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// Spawn a new Claude session for `cwd`, returning the tmux session name.
pub fn spawn_claude_session(cwd: &str) -> io::Result<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let name = unique_session_name("cchub");
    create_detached_tmux_session(&name, cwd, &format!("{} -ic ccyo", shell))?;
    Ok(name)
}

/// Spawn an ephemeral tmux session that runs an interactive shell in `cwd`.
/// Pair with [`crate::tmux_pane::TmuxPaneView::spawn_owned`] so closing the
/// popup tears the session down.
pub fn spawn_shell_tmux_session(cwd: &str) -> io::Result<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let name = unique_session_name("cchub-sh");
    create_detached_tmux_session(&name, cwd, &shell)?;
    Ok(name)
}

/// Open a new terminal window that attaches to an existing detached tmux
/// session. Used to recover a session whose terminal was closed — the agent
/// kept running headlessly, this brings it back into view.
///
/// Unlike the `ccyo`-spawn path, this execs `tmux attach` directly as the
/// terminal's child (no intervening shell). A user shell rc that auto-starts
/// tmux on every interactive shell would otherwise hijack `zsh -ic 'tmux
/// attach …'` and never run the attach.
pub fn attach_tmux_session(tmux_name: &str, cwd: &str) -> io::Result<String> {
    let launcher = terminal::pick().ok_or_else(|| {
        io::Error::other("no terminal emulator found (set $TERMINAL or install kitty/foot/alacritty)")
    })?;
    let argv = launcher.argv_bare(cwd, &["tmux", "attach", "-t", tmux_name]);
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

fn unique_session_name(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}-{}", prefix, std::process::id(), nanos)
}

fn create_detached_tmux_session(name: &str, cwd: &str, shell_cmd: &str) -> io::Result<()> {
    let args = ["new-session", "-d", "-s", name, "-c", cwd, shell_cmd];
    info!("spawn: tmux {}", args.join(" "));

    let output = Command::new("tmux").args(args).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        error!("spawn: new-session failed: {}", stderr);
        return Err(io::Error::other(format!(
            "tmux new-session failed: {}",
            if stderr.is_empty() {
                output.status.to_string()
            } else {
                stderr
            }
        )));
    }

    // Session-scoped so we don't flip the user's global mouse setting.
    send::enable_session_mouse(name);
    Ok(())
}

