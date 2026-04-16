/// Platform-specific process inspection.
///
/// Linux reads `/proc/<pid>/{stat,comm}`; macOS uses `libproc` because
/// Darwin has no procfs. Call via the `Process` alias — e.g.
/// `Process::parent_pid(pid)` — and the right impl is selected at
/// compile time.
pub trait ProcessInfo {
    fn parent_pid(pid: u32) -> Option<u32>;
    fn name(pid: u32) -> String;
    fn session_id(pid: u32) -> Option<u32>;

    /// True when the given PID is a live Claude Code process. Linux identifies
    /// by `comm == "claude"`; macOS checks the executable path for a
    /// `claude/versions/` segment, since Claude Code's macOS install names
    /// each version binary literally (e.g. `2.1.112`) rather than `claude`.
    fn is_claude(pid: u32) -> bool;
}

#[cfg(target_os = "linux")]
mod imp {
    use super::ProcessInfo;
    use std::fs;

    pub struct Platform;

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

    impl ProcessInfo for Platform {
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
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::ProcessInfo;
    use libproc::bsd_info::BSDInfo;
    use libproc::proc_pid;
    use std::path::Path;

    pub struct Platform;

    impl ProcessInfo for Platform {
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
    }
}

pub use imp::Platform;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_real_ppid_and_name_for_current_process() {
        let pid = std::process::id();
        let ppid = Platform::parent_pid(pid).expect("parent_pid should work for self");
        assert!(ppid > 1, "expected real parent, got {}", ppid);

        let name = Platform::name(pid);
        assert!(!name.is_empty(), "process name should not be empty");
    }
}
