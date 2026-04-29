//! Process inspection (parent pid, name, session, liveness, and agent checks).
//!
//! Linux reads `/proc/<pid>/{stat,comm}`; macOS uses `libproc` because
//! Darwin has no procfs. Call via the `Process` alias — e.g.
//! `Process::parent_pid(pid)` — and the right impl is selected at
//! compile time.

use crate::agent::AgentKind;

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
            path.contains("/claude/versions/")
        }

        fn is_alive(pid: u32) -> bool {
            unsafe { libc::kill(pid as i32, 0) == 0 }
        }
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::ProcessInfo;
    use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE, STILL_ACTIVE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    pub struct Process;

    fn with_entries<T>(mut f: impl FnMut(&PROCESSENTRY32W) -> Option<T>) -> Option<T> {
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == 0 as HANDLE || snap as isize == -1 {
                return None;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap, &mut entry);
            let mut out = None;
            while ok != FALSE {
                if let Some(v) = f(&entry) {
                    out = Some(v);
                    break;
                }
                ok = Process32NextW(snap, &mut entry);
            }
            CloseHandle(snap);
            out
        }
    }

    fn exe_name(entry: &PROCESSENTRY32W) -> String {
        let len = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        String::from_utf16_lossy(&entry.szExeFile[..len])
    }

    impl ProcessInfo for Process {
        fn parent_pid(pid: u32) -> Option<u32> {
            with_entries(|e| (e.th32ProcessID == pid).then_some(e.th32ParentProcessID))
        }

        fn name(pid: u32) -> String {
            with_entries(|e| (e.th32ProcessID == pid).then(|| exe_name(e))).unwrap_or_default()
        }

        fn session_id(_pid: u32) -> Option<u32> {
            None
        }

        fn is_claude(pid: u32) -> bool {
            let n = <Self as ProcessInfo>::name(pid).to_ascii_lowercase();
            n == "claude.exe" || n == "claude"
        }

        fn is_alive(pid: u32) -> bool {
            unsafe {
                let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
                if h == 0 as HANDLE {
                    return false;
                }
                let mut code: u32 = 0;
                let ok = GetExitCodeProcess(h, &mut code) != FALSE;
                CloseHandle(h);
                ok && code == STILL_ACTIVE as u32
            }
        }
    }
}

pub use imp::Process;

use log::debug;

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

/// Pure detector for Pi-agent invocations from `comm` + `cmdline`. Both
/// inputs must be lowercased. Exact-basename match on the first cmdline
/// argument prevents `/usr/bin/pipewire` from substring-matching `/bin/pi`.
fn matches_pi_command(name: &str, cmd: &str) -> bool {
    if name == "pi" || name == "pi.exe" {
        return true;
    }
    let exe_basename = cmd
        .split_whitespace()
        .next()
        .and_then(|first| std::path::Path::new(first).file_name())
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if exe_basename == "pi" || exe_basename == "pi.exe" {
        return true;
    }
    cmd.contains("pi-coding-agent") || cmd.contains("@mariozechner/pi-coding-agent")
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
    }
}

#[cfg(target_os = "linux")]
pub fn command_line(pid: u32) -> String {
    std::fs::read(format!("/proc/{}/cmdline", pid))
        .ok()
        .map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .replace('\0', " ")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

#[cfg(not(target_os = "linux"))]
pub fn command_line(_pid: u32) -> String {
    String::new()
}

#[cfg(target_os = "linux")]
pub fn current_dir(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{}/cwd", pid))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(not(target_os = "linux"))]
pub fn current_dir(_pid: u32) -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
pub fn list_pids() -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .collect()
}

#[cfg(target_os = "windows")]
pub fn list_pids() -> Vec<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == 0 as HANDLE || snap as isize == -1 {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut entry);
        while ok != FALSE {
            out.push(entry.th32ProcessID);
            ok = Process32NextW(snap, &mut entry);
        }
        CloseHandle(snap);
        out
    }
}

#[cfg(target_os = "macos")]
pub fn list_pids() -> Vec<u32> {
    Vec::new()
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
    fn pi_detector_matches_real_pi_invocations() {
        assert!(matches_pi_command("pi", ""));
        assert!(matches_pi_command("pi.exe", ""));
        assert!(matches_pi_command(
            "node",
            "/usr/local/bin/pi --provider openai-codex --model gpt-5.4"
        ));
        assert!(matches_pi_command(
            "node",
            "node /home/u/.npm/_npx/abc/node_modules/pi-coding-agent/dist/cli.js"
        ));
        assert!(matches_pi_command(
            "node",
            "node /home/u/.npm/_npx/abc/node_modules/@mariozechner/pi-coding-agent/cli.js"
        ));
    }

    #[test]
    fn pi_detector_rejects_pipewire() {
        // Regression: `/usr/bin/pipewire` once substring-matched `/bin/pi`.
        assert!(!matches_pi_command("pipewire", "/usr/bin/pipewire"));
        assert!(!matches_pi_command(
            "pipewire-pulse",
            "/usr/bin/pipewire-pulse"
        ));
        assert!(!matches_pi_command("ping", "/usr/bin/ping 8.8.8.8"));
    }
}
