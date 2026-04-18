//! Process inspection (parent pid, name, session, is-claude, liveness).
//!
//! Linux reads `/proc/<pid>/{stat,comm}`; macOS uses `libproc` because
//! Darwin has no procfs. Call via the `Process` alias — e.g.
//! `Process::parent_pid(pid)` — and the right impl is selected at
//! compile time.

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

#[cfg(target_os = "linux")]
mod imp {
    use super::ProcessInfo;
    use std::fs;

    pub struct Process;

    fn stat_fields(pid: u32) -> Option<Vec<String>> {
        let stat = fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
        // /proc/<pid>/stat format: pid (comm) state ppid pgrp session ...
        // comm can contain parens/spaces, so find last ')' first
        let after_comm = stat.rfind(')')? + 2;
        Some(
            stat[after_comm..]
                .split_whitespace()
                .map(String::from)
                .collect(),
        )
    }

    impl ProcessInfo for Process {
        fn parent_pid(pid: u32) -> Option<u32> {
            stat_fields(pid)?.get(1)?.parse().ok()
        }

        fn name(pid: u32) -> String {
            fs::read_to_string(format!("/proc/{}/comm", pid))
                .unwrap_or_default()
                .trim()
                .to_string()
        }

        fn session_id(pid: u32) -> Option<u32> {
            stat_fields(pid)?.get(3)?.parse().ok()
        }

        fn is_claude(pid: u32) -> bool {
            <Self as ProcessInfo>::name(pid) == "claude"
        }

        fn is_alive(pid: u32) -> bool {
            unsafe { libc::kill(pid as i32, 0) == 0 }
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::ProcessInfo;
    use libproc::bsd_info::BSDInfo;
    use libproc::proc_pid;
    use std::path::Path;

    pub struct Process;

    impl ProcessInfo for Process {
        fn parent_pid(pid: u32) -> Option<u32> {
            proc_pid::pidinfo::<BSDInfo>(pid as i32, 0)
                .ok()
                .map(|info| info.pbi_ppid)
        }

        /// Returns the basename of the executable path (what `ps -o comm`
        /// shows on macOS, matching Linux `/proc/<pid>/comm`). We avoid
        /// `proc_pid::name` and `pbi_comm` because processes like Claude
        /// Code overwrite those via setproctitle (e.g. to "2.1.112").
        fn name(pid: u32) -> String {
            let Ok(path) = proc_pid::pidpath(pid as i32) else {
                return String::new();
            };
            Path::new(&path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string()
        }

        fn session_id(pid: u32) -> Option<u32> {
            let sid = unsafe { libc::getsid(pid as i32) };
            if sid < 0 {
                None
            } else {
                Some(sid as u32)
            }
        }

        fn is_claude(pid: u32) -> bool {
            let Ok(path) = proc_pid::pidpath(pid as i32) else {
                return false;
            };
            // Claude Code installs each version as its own binary under
            // `.../claude/versions/<version>`, so the binary's basename is a
            // version string and the parent segment is `versions` with
            // grandparent `claude`. Match on the path segments.
            path.contains("/claude/versions/")
        }

        fn is_alive(pid: u32) -> bool {
            unsafe { libc::kill(pid as i32, 0) == 0 }
        }
    }
}

pub use imp::Process;

use log::debug;

/// Walk the process tree upward from `start`, appending ancestor PIDs to
/// `pids`. Stops at init (ppid ≤ 1).
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

/// PID chain for window lookups: `pid` followed by every ancestor up to init.
/// Falls back to the session leader when the direct walk stalls at init
/// (orphaned/reparented process), because the session leader's parent is
/// usually the terminal emulator window we care about.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_real_ppid_and_name_for_current_process() {
        let pid = std::process::id();
        let ppid = Process::parent_pid(pid).expect("parent_pid should work for self");
        assert!(ppid > 1, "expected real parent, got {}", ppid);

        let name = Process::name(pid);
        assert!(!name.is_empty(), "process name should not be empty");
    }
}
