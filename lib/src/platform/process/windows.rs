//! Windows: Toolhelp process snapshots and process handles.

use super::ProcessInfo;
use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE, STILL_ACTIVE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE,
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

pub fn terminate(pid: u32) -> bool {
    if pid <= 1 || pid == std::process::id() {
        return false;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle.is_null() {
            return false;
        }
        let ok = TerminateProcess(handle, 0) != 0;
        CloseHandle(handle);
        ok
    }
}

pub fn list_pids() -> Vec<u32> {
    let mut out = Vec::new();
    with_entries(|e| {
        out.push(e.th32ProcessID);
        None::<()>
    });
    out
}
