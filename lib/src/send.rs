//! Dispatch a prompt into a running Claude Code session via the multiplexer.
//!
//! The session must live inside a multiplexer pane (either because cc-hub
//! spawned it via [`crate::spawn`], or because the user set it up that way
//! by hand). We find the pane by matching the Claude PID's ancestor chain
//! against the mux's pane list, then inject the prompt with `send-keys`.
//!
//! [`crate::platform::mux`] picks tmux on Unix and psmux on Windows — this
//! module doesn't care which.

use crate::platform::{mux, process};
use std::io;

/// Find the multiplexer session name hosting the Claude Code process `pid`.
///
/// Walks the pid's ancestor chain and matches against the `pane_pid` of
/// every pane the mux knows about. Returns `None` when no ancestor is a
/// pane leader — meaning the session is not in a multiplexer (e.g. launched
/// before the spawn refactor).
pub fn tmux_session_for_pid(pid: u32) -> Option<String> {
    tmux_session_for_pid_in(pid, &tmux_panes())
}

/// Type `text` into the session as if the user had typed it, then send a
/// literal Enter to submit.
pub fn send_prompt(session: &str, text: &str) -> io::Result<()> {
    mux::send_prompt(session, text)
}

/// Best-effort check that `session`'s pane is showing claude's input
/// prompt and is ready to accept a paste. See [`mux::pane_ready_for_input`]
/// for the rationale.
pub fn pane_ready_for_input(session: &str) -> bool {
    mux::pane_ready_for_input(session)
}

/// PIDs of clients attached to `session_name` — the processes that ran
/// `attach`. Their ancestor chain reaches the terminal window, which is
/// how focus/close locate the window hosting a detached agent.
///
/// Returns empty on Windows (psmux doesn't honour the `-F` format string
/// used by this lookup; see [`crate::platform::mux`] for the tradeoff).
pub fn tmux_client_pids(session_name: &str) -> Vec<u32> {
    mux::client_pids(session_name)
}

/// Kill the multiplexer session `session_name`. Ends any `cc-hub-new`/claude
/// processes running inside and causes attached clients to exit (closing
/// their terminal windows).
pub fn kill_tmux_session(session_name: &str) -> io::Result<()> {
    mux::kill_session(session_name)
}

/// Turn on the multiplexer's `mouse` option for `session_name`. Best-effort:
/// failure is logged but not returned — the caller treats missing mouse as a
/// degraded experience, not a spawn failure.
pub fn enable_session_mouse(session_name: &str) {
    mux::enable_mouse(session_name);
}

/// Snapshot of all multiplexer panes. Callers that need to look up many
/// sessions at once should take one snapshot and pass it to
/// [`tmux_session_for_pid_in`] instead of re-shelling per candidate.
pub fn tmux_panes() -> Vec<(u32, String)> {
    mux::list_panes()
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
