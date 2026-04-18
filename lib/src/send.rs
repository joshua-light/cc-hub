//! Dispatch a prompt into a running Claude Code session via tmux.
//!
//! The session must live inside a tmux pane (either because cc-hub spawned
//! it via [`crate::spawn`], or because the user set it up that way by hand).
//! We find the pane by matching the Claude PID's ancestor chain against
//! `tmux list-panes`, then inject the prompt with `tmux send-keys`.

use crate::platform::process;
use log::{error, info, warn};
use std::io;
use std::process::Command;

/// Find the tmux session name hosting the Claude Code process `pid`.
///
/// Walks the pid's ancestor chain and matches against the `pane_pid` of
/// every pane tmux knows about. Returns `None` when no ancestor is a tmux
/// pane leader — meaning the session is not in tmux (e.g. launched before
/// the spawn refactor).
pub fn tmux_session_for_pid(pid: u32) -> Option<String> {
    tmux_session_for_pid_in(pid, &list_panes()?)
}

/// Type `text` into the tmux session as if the user had typed it, then
/// send a literal Enter to submit. `-l` (literal) prevents `$`, `;`, etc.
/// from being interpreted as tmux key names.
pub fn send_prompt(tmux_session: &str, text: &str) -> io::Result<()> {
    info!(
        "send: tmux session={} text_len={}",
        tmux_session,
        text.len()
    );
    run_tmux(&["send-keys", "-t", tmux_session, "-l", text], "literal")?;
    // Separate call — `-l` would type the literal word "Enter".
    run_tmux(&["send-keys", "-t", tmux_session, "Enter"], "Enter")?;
    Ok(())
}

fn run_tmux(args: &[&str], label: &str) -> io::Result<()> {
    let out = Command::new("tmux").args(args).output()?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    error!("send: send-keys {} failed: {}", label, stderr);
    Err(io::Error::other(format!(
        "tmux send-keys {} failed: {}",
        label,
        if stderr.is_empty() {
            out.status.to_string()
        } else {
            stderr
        }
    )))
}

/// PIDs of tmux clients attached to `session_name` — the processes that ran
/// `tmux attach`. Their ancestor chain reaches the terminal window, which is
/// how focus/close locate the window hosting a detached-tmux agent (the
/// agent's own process tree lives under the tmux server, reparented to init,
/// so it doesn't touch any window).
pub fn tmux_client_pids(session_name: &str) -> Vec<u32> {
    let Ok(out) = Command::new("tmux")
        .args(["list-clients", "-t", session_name, "-F", "#{client_pid}"])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

/// Kill the tmux session `session_name`. Ends any `ccyo`/claude processes
/// running inside and causes attached clients to exit (closing their
/// terminal windows).
pub fn kill_tmux_session(session_name: &str) -> io::Result<()> {
    run_tmux(&["kill-session", "-t", session_name], "kill-session")
}

/// Snapshot of all tmux panes. Callers that need to look up many sessions
/// at once should take one snapshot and pass it to
/// [`tmux_session_for_pid_in`] instead of re-shelling per candidate.
pub fn tmux_panes() -> Vec<(u32, String)> {
    list_panes().unwrap_or_default()
}

/// Like [`tmux_session_for_pid`] but uses a caller-provided `panes` snapshot.
pub fn tmux_session_for_pid_in(pid: u32, panes: &[(u32, String)]) -> Option<String> {
    let chain = process::collect_pid_chain(pid);
    chain.iter().find_map(|ancestor| {
        panes
            .iter()
            .find_map(|(pane_pid, name)| (pane_pid == ancestor).then(|| name.clone()))
    })
}

/// Query tmux for `(pane_pid, session_name)` of every pane in every session.
/// Returns `None` when tmux is missing or the command fails (no server running,
/// permissions, etc.).
fn list_panes() -> Option<Vec<(u32, String)>> {
    let out = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_pid} #{session_name}"])
        .output()
        .ok()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        warn!(
            "send: tmux list-panes failed status={} stderr={:?}",
            out.status,
            stderr.trim()
        );
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let panes: Vec<(u32, String)> = stdout
        .lines()
        .filter_map(|line| {
            let (pid_s, name) = line.split_once(' ')?;
            let pid: u32 = pid_s.parse().ok()?;
            Some((pid, name.to_string()))
        })
        .collect();
    Some(panes)
}
