//! Spawn a new `ccyo` session in a fresh terminal window (or tmux window).
//!
//! Strategy:
//! - In tmux: open a sibling `new-window`, no terminal spawn.
//! - Otherwise: pick a [`terminal::Launcher`] via
//!   [`crate::platform::terminal::pick`] and hand the spawn to the detected
//!   [`window::WindowManager`] for workspace placement; fall back to a direct
//!   `Command::spawn` when no WM can place it.

use crate::platform::{paths, terminal, window};
use log::{error, info};
use std::io;
use std::process::{Command, Stdio};

/// Spawn a new Claude session for `cwd`.
///
/// `pid_hint` is the pid of the currently-selected session; when provided,
/// we ask the window manager which workspace that window is on so the new
/// window lands there without pulling focus from cc-hub.
pub fn spawn_claude_session(cwd: &str, pid_hint: Option<u32>) -> io::Result<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());

    if std::env::var("TMUX").is_ok() {
        spawn_tmux_window(cwd, &shell)
    } else {
        let workspace = pid_hint.and_then(crate::focus::workspace_for_pid);
        spawn_terminal(cwd, &shell, workspace.as_deref())
    }
}

fn spawn_tmux_window(cwd: &str, shell: &str) -> io::Result<String> {
    let shell_cmd = format!("{} -ic ccyo", shell);
    let args = [
        "new-window",
        "-c",
        cwd,
        "-n",
        "claude",
        "-P",
        "-F",
        "#{session_name}:#{window_index}",
        &shell_cmd,
    ];
    info!("spawn: tmux {}", args.join(" "));

    let output = Command::new("tmux").args(args).output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    info!(
        "spawn: new-window status={} stdout={:?} stderr={:?}",
        output.status, stdout, stderr
    );
    if !output.status.success() {
        error!("spawn: new-window failed: {}", stderr);
        return Err(io::Error::other(format!(
            "tmux new-window failed: {}",
            if stderr.is_empty() { output.status.to_string() } else { stderr }
        )));
    }
    Ok(format!("spawned ccyo in tmux window {}", stdout))
}

fn spawn_terminal(cwd: &str, shell: &str, workspace: Option<&str>) -> io::Result<String> {
    let launcher = terminal::pick().ok_or_else(|| {
        io::Error::other("no terminal emulator found (set $TERMINAL or install kitty/foot/alacritty)")
    })?;

    let argv = launcher.argv(cwd, shell);
    // Prefer a user-provided Hyprland wrapper script when one exists — users
    // with ~/.config/hypr/scripts/<term> tend to pass a personal --config-file
    // and expect our spawned terminals to look the same as their SUPER+Enter
    // ones. Harmless on non-Hyprland hosts (the file just won't exist).
    let bin = paths::terminal_wrapper_script(launcher.name())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| launcher.name().to_string());

    if let Some(ws) = workspace {
        match window::current().spawn_on_workspace(ws, &bin, &argv) {
            Ok(()) => {
                return Ok(format!("spawned ccyo in {} on workspace {}", launcher.name(), ws));
            }
            Err(e) => {
                error!("spawn: workspace placement failed ({}), falling back", e);
            }
        }
    }

    info!("spawn: {} {}", bin, argv.join(" "));
    let child = Command::new(&bin)
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    match child {
        Ok(c) => {
            info!("spawn: {} pid={}", launcher.name(), c.id());
            Ok(format!("spawned ccyo in {} window", launcher.name()))
        }
        Err(e) => {
            error!("spawn: {} failed: {}", launcher.name(), e);
            Err(e)
        }
    }
}
