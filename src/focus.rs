//! Focus / close the terminal window that owns a given pid.
//!
//! Two flavours of lookup:
//!
//! - **Detached-tmux agents** (what cc-hub spawns today): the Claude process
//!   tree lives under the tmux server (reparented to init), so walking up
//!   from the Claude pid never reaches a window. We ask tmux for the
//!   session's *client* pids — those are the `tmux attach` processes, which
//!   are children of the terminal's shell — and walk up from them instead.
//!   Close kills the tmux session (which ends the agent and detaches any
//!   clients, closing their windows).
//!
//! - **Non-tmux agents** (e.g. sessions launched before the tmux refactor):
//!   fall back to walking up from the Claude pid itself.

use crate::platform::{process, window};
use crate::send;
use log::{debug, info, warn};

pub enum FocusOutcome {
    Focused,
    /// Tmux-backed session with no attached client — caller should spawn a
    /// terminal that `tmux attach`es (see [`crate::spawn::attach_tmux_session`]).
    NeedsReattach(String),
    Failed(String),
}

pub fn focus_window(pid: u32) -> FocusOutcome {
    info!("focus_window called for pid={}", pid);
    if let Some(name) = send::tmux_session_for_pid(pid) {
        let pids = tmux_client_chain(&name);
        if pids.is_empty() {
            info!("focus: tmux session {} has no clients", name);
            return FocusOutcome::NeedsReattach(name);
        }
        info!("focus: tmux client chain for {}: {:?}", name, pids);
        return if window::current().focus(&pids) {
            FocusOutcome::Focused
        } else {
            FocusOutcome::Failed(format!("no window matched client chain for {}", name))
        };
    }
    let pids = process::collect_pid_chain(pid);
    info!("focus: pid chain: {:?}", pids);
    if window::current().focus(&pids) {
        FocusOutcome::Focused
    } else {
        warn!("no window found for pid {} or any ancestor", pid);
        FocusOutcome::Failed(format!("no window for PID {}", pid))
    }
}

/// Ask the terminal window owning `pid` to close.
///
/// For tmux-backed sessions this kills the whole tmux session, so the agent
/// exits and any attached terminals close with it. For other sessions it
/// sends the WM's graceful close (terminal exits → SIGHUP to the claude
/// process group). Returns true on success.
pub fn close_window(pid: u32) -> bool {
    info!("close_window called for pid={}", pid);
    if let Some(name) = send::tmux_session_for_pid(pid) {
        match send::kill_tmux_session(&name) {
            Ok(()) => {
                info!("close: killed tmux session {}", name);
                return true;
            }
            Err(e) => {
                warn!("close: kill-session {} failed: {}", name, e);
                // Fall through to window-close as a last resort.
            }
        }
    }
    let pids = process::collect_pid_chain(pid);
    info!("window lookup chain: {:?}", pids);
    let ok = window::current().close(&pids);
    if !ok {
        warn!("no window found to close for pid {} or any ancestor", pid);
    }
    ok
}

/// Workspace identifier for the window owning `pid` (opaque string understood
/// by the detected WM). None when no WM knows about the pid or no WM supports
/// workspaces.
pub fn workspace_for_pid(pid: u32) -> Option<String> {
    let pids = window_lookup_pids(pid);
    window::current().workspace_for_pids(&pids)
}

/// PIDs to hand to `WindowManager::{focus, workspace_for_pids}`. Prefers
/// tmux client pids (which live under the terminal window); falls back to
/// the Claude pid's own ancestor chain for non-tmux sessions.
fn window_lookup_pids(pid: u32) -> Vec<u32> {
    if let Some(name) = send::tmux_session_for_pid(pid) {
        let pids = tmux_client_chain(&name);
        if !pids.is_empty() {
            debug!("tmux client chain for {}: {:?}", name, pids);
            return pids;
        }
        debug!("no tmux clients attached to {}, falling back", name);
    }
    process::collect_pid_chain(pid)
}

/// Attached-client pids for `name` plus each client's ancestor chain, which
/// reaches the terminal window hosting the `tmux attach`. Empty when the
/// session has no attached clients.
fn tmux_client_chain(name: &str) -> Vec<u32> {
    let clients = send::tmux_client_pids(name);
    let mut pids: Vec<u32> = Vec::new();
    for c in clients {
        pids.push(c);
        process::walk_ancestors(&mut pids, c, "client");
    }
    pids
}
