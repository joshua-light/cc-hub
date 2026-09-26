//! Process inspection: parent pid, name, session, liveness, and agent checks.
//!
//! Call through the [`Process`] alias (e.g. `Process::parent_pid(pid)`); the
//! right OS impl is selected at compile time.
//!
//! - `linux`: procfs.
//! - `macos`: `libproc` and sysctl, since Darwin has no procfs.
//! - `windows`: Toolhelp snapshots and process handles.
//! - [`agent_cmd`]: pure classifiers for agent command lines.

use crate::agent::AgentKind;
use log::debug;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use std::collections::HashMap;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use std::path::PathBuf;

mod agent_cmd;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use agent_cmd::{codex_is_interactive, matches_codex_command, matches_pi_command};
pub use agent_cmd::{codex_model_arg, codex_resume_session_arg};
#[cfg(target_os = "linux")]
pub use linux::{command_line, current_dir, list_pids, open_codex_rollouts, Process};
#[cfg(target_os = "macos")]
pub use macos::{command_line, current_dir, list_pids, open_codex_rollouts, Process};
#[cfg(target_os = "windows")]
pub use windows::{list_pids, terminate, Process};

pub trait ProcessInfo {
    fn parent_pid(pid: u32) -> Option<u32>;
    fn name(pid: u32) -> String;
    fn session_id(pid: u32) -> Option<u32>;

    /// True when the given PID is a live Claude Code process. Linux identifies
    /// by `comm == "claude"`; macOS checks the executable path for a
    /// `claude/versions/` segment, since Claude Code's macOS install names
    /// each version binary literally (e.g. `2.1.112`) rather than `claude`.
    fn is_claude(pid: u32) -> bool;

    /// Signal-0 liveness check. Returns true if the PID exists and the current
    /// process has permission to signal it.
    fn is_alive(pid: u32) -> bool;
}

pub fn walk_ancestors(pids: &mut Vec<u32>, start: u32, label: &str) {
    let mut current = start;
    while let Some(ppid) = Process::parent_pid(current) {
        if ppid <= 1 {
            debug!("reached init (ppid={}), stopping {} walk", ppid, label);
            break;
        }
        let comm = Process::name(ppid);
        debug!("  {} {} -> parent {} ({})", label, current, ppid, comm);
        pids.push(ppid);
        current = ppid;
    }
}

pub fn collect_pid_chain(pid: u32) -> Vec<u32> {
    let mut pids = vec![pid];
    walk_ancestors(&mut pids, pid, "pid");

    if pids.len() <= 1 {
        if let Some(sid) = Process::session_id(pid) {
            if sid != pid && sid > 1 {
                debug!(
                    "pid {} reparented to init, falling back to session leader {}",
                    pid, sid
                );
                pids.push(sid);
                walk_ancestors(&mut pids, sid, "sid");
            }
        }
    }

    pids
}

pub fn is_agent_process(kind: AgentKind, pid: u32) -> bool {
    if !Process::is_alive(pid) {
        return false;
    }
    match kind {
        AgentKind::Claude => Process::is_claude(pid),
        AgentKind::Pi => {
            let cmd = command_line(pid).to_ascii_lowercase();
            let name = Process::name(pid).to_ascii_lowercase();
            matches_pi_command(&name, &cmd)
        }
        AgentKind::Codex => {
            let name = Process::name(pid).to_ascii_lowercase();
            if !matches_codex_command(&name) {
                return false;
            }
            // An interactive TUI session only — never a background `app-server`
            // / `mcp-server`, a one-shot `codex exec`, or a ChatGPT-app helper.
            let cmd = command_line(pid).to_ascii_lowercase();
            codex_is_interactive(&cmd)
        }
    }
}

/// Ask one agent process to terminate gracefully.
///
/// This deliberately signals only `pid`, not its process group: old
/// non-tmux sessions share a terminal with their parent shell, and "remove
/// session" must never close or signal that shell when window automation is
/// unavailable.
#[cfg(unix)]
pub fn terminate(pid: u32) -> bool {
    if pid <= 1 || pid == std::process::id() {
        return false;
    }
    unsafe { libc::kill(pid as i32, libc::SIGTERM) == 0 }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn command_line(_pid: u32) -> String {
    String::new()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn current_dir(_pid: u32) -> Option<String> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn open_codex_rollouts(_pids: &[u32], _candidates: &[PathBuf]) -> HashMap<u32, PathBuf> {
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_real_ppid_and_name_for_current_process() {
        let pid = std::process::id();
        assert!(Process::parent_pid(pid).is_some());
        assert!(!Process::name(pid).is_empty());
    }

    #[test]
    fn terminate_rejects_current_process() {
        assert!(!terminate(std::process::id()));
    }

    #[cfg(unix)]
    #[test]
    fn terminate_stops_only_target_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn child");

        assert!(terminate(child.id()));
        let status = child.wait().expect("wait for terminated child");
        assert!(!status.success());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn open_rollout_discovery_finds_held_file() {
        let file = tempfile::NamedTempFile::new().expect("temp rollout");
        let path = file.path().to_path_buf();
        let pid = std::process::id();

        let owners = open_codex_rollouts(&[pid], std::slice::from_ref(&path));

        assert_eq!(owners.get(&pid), Some(&path));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn list_pids_includes_self() {
        let pid = std::process::id();
        let pids = list_pids();
        assert!(!pids.is_empty(), "list_pids returned empty");
        assert!(pids.contains(&pid), "list_pids missing self pid {}", pid);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn current_dir_matches_self() {
        let pid = std::process::id();
        let got = current_dir(pid).expect("current_dir returned None");
        let expected = std::env::current_dir().unwrap();
        // canonicalize both to neutralise /private symlinks on macOS.
        let got_c = std::fs::canonicalize(&got).unwrap_or_else(|_| got.clone().into());
        let exp_c = std::fs::canonicalize(&expected).unwrap_or(expected);
        assert_eq!(got_c, exp_c, "current_dir mismatch (raw={})", got);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn command_line_contains_self_argv0() {
        let pid = std::process::id();
        let cmd = command_line(pid);
        assert!(!cmd.is_empty(), "command_line returned empty");
        let argv0 = std::env::args().next().unwrap_or_default();
        let basename = std::path::Path::new(&argv0)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        assert!(
            !basename.is_empty() && cmd.contains(basename),
            "command_line {:?} does not contain argv0 basename {:?}",
            cmd,
            basename
        );
    }
}
