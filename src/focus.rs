//! Focus / close the terminal window that owns a given pid.
//!
//! The pid-chain walk is platform-generic (uses [`crate::platform::process`]);
//! the actual window operations are delegated to whichever
//! [`crate::platform::window::WindowManager`] was detected at startup.

use crate::platform::{process, window};
use log::{info, warn};

pub fn focus_window(pid: u32) {
    info!("focus_window called for pid={}", pid);
    let pids = process::collect_pid_chain(pid);
    info!("process chain: {:?}", pids);
    if !window::current().focus(&pids) {
        warn!("no window found for pid {} or any ancestor", pid);
    }
}

/// Ask the terminal window owning `pid` to close. Returns true on success.
/// The window manager sends a graceful close — the terminal then exits and
/// SIGHUPs its foreground process group (killing the Claude session).
pub fn close_window(pid: u32) -> bool {
    info!("close_window called for pid={}", pid);
    let pids = process::collect_pid_chain(pid);
    info!("process chain: {:?}", pids);
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
    let pids = process::collect_pid_chain(pid);
    window::current().workspace_for_pids(&pids)
}
