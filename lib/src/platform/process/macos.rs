//! macOS: `libproc` and sysctl, since Darwin has no procfs.

use super::ProcessInfo;
use libproc::bsd_info::BSDInfo;
use libproc::proc_pid;
use log::debug;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Process;

/// `ps` fallback for [`Process::parent_pid`]. `libproc::pidinfo` returns
/// `Err` for some processes the caller doesn't own — notably
/// `/usr/bin/login`, which is setuid-root and which kitty / Terminal.app
/// insert between the emulator and the user's shell. Without this
/// fallback, [`super::walk_ancestors`] stops at the login process and
/// never reaches the terminal emulator that owns the on-screen window.
/// `ps` reads ppids through sysctl(`KERN_PROC_PID`), which has no such
/// restriction.
fn parent_pid_ps(pid: u32) -> Option<u32> {
    let out = Command::new("/bin/ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim().parse::<u32>().ok()
}

impl ProcessInfo for Process {
    fn parent_pid(pid: u32) -> Option<u32> {
        if let Ok(info) = proc_pid::pidinfo::<BSDInfo>(pid as i32, 0) {
            return Some(info.pbi_ppid);
        }
        // libproc denied us (cross-user / setuid-root). Try ps.
        let p = parent_pid_ps(pid);
        debug!("parent_pid: libproc failed for pid {}, ps -> {:?}", pid, p);
        p
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

pub fn command_line(pid: u32) -> String {
    // KERN_PROCARGS2 returns: argc (i32) | exec_path (NUL-term, padded) |
    // argv[0..argc] (NUL-separated) | envp... — see Apple's `ps` source.
    // Buffer size is bounded by `kern.argmax`; querying it is one sysctl, so
    // do that instead of guessing.
    use std::mem;

    let mut argmax: libc::c_int = 0;
    let mut argmax_size = mem::size_of::<libc::c_int>();
    let mut mib_argmax = [libc::CTL_KERN, libc::KERN_ARGMAX];
    let rc = unsafe {
        libc::sysctl(
            mib_argmax.as_mut_ptr(),
            mib_argmax.len() as u32,
            &mut argmax as *mut _ as *mut libc::c_void,
            &mut argmax_size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || argmax <= 0 {
        return String::new();
    }

    let mut buf: Vec<u8> = vec![0; argmax as usize];
    let mut size = buf.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size < mem::size_of::<libc::c_int>() {
        return String::new();
    }
    buf.truncate(size);

    let argc = i32::from_ne_bytes(buf[..4].try_into().unwrap_or([0; 4]));
    if argc <= 0 {
        return String::new();
    }

    // Skip argc, then the exec_path C string (with any trailing alignment NULs).
    let mut i = 4;
    while i < buf.len() && buf[i] != 0 {
        i += 1;
    }
    while i < buf.len() && buf[i] == 0 {
        i += 1;
    }

    let mut args: Vec<String> = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        if i >= buf.len() {
            break;
        }
        let start = i;
        while i < buf.len() && buf[i] != 0 {
            i += 1;
        }
        args.push(String::from_utf8_lossy(&buf[start..i]).into_owned());
        i += 1;
    }
    args.join(" ")
}

pub fn current_dir(pid: u32) -> Option<String> {
    // proc_pidinfo(pid, PROC_PIDVNODEPATHINFO=9, 0, &info, sizeof(info)) returns
    // a `proc_vnodepathinfo` whose `pvi_cdir.vip_path` is a 1024-byte NUL-
    // terminated path. The struct itself is two `vnode_info_path`s
    // (cdir, rdir); we only read the first 1024-byte path field, located
    // immediately after the 152-byte `vnode_info` header. The layout is
    // ABI-stable XNU.
    const PROC_PIDVNODEPATHINFO: libc::c_int = 9;
    const VNODE_INFO_SIZE: usize = 152;
    const VIP_PATH_LEN: usize = 1024;
    const PROC_VNODEPATHINFO_SIZE: usize = (VNODE_INFO_SIZE + VIP_PATH_LEN) * 2;

    extern "C" {
        fn proc_pidinfo(
            pid: libc::c_int,
            flavor: libc::c_int,
            arg: u64,
            buffer: *mut libc::c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }

    let mut buf = [0u8; PROC_VNODEPATHINFO_SIZE];
    let rc = unsafe {
        proc_pidinfo(
            pid as libc::c_int,
            PROC_PIDVNODEPATHINFO,
            0,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len() as libc::c_int,
        )
    };
    if rc <= 0 {
        return None;
    }
    let path_bytes = &buf[VNODE_INFO_SIZE..VNODE_INFO_SIZE + VIP_PATH_LEN];
    let nul = path_bytes.iter().position(|&b| b == 0).unwrap_or(0);
    if nul == 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&path_bytes[..nul]).into_owned())
}

pub fn list_pids() -> Vec<u32> {
    use libproc::processes::{pids_by_type, ProcFilter};
    pids_by_type(ProcFilter::All).unwrap_or_default()
}

/// Resolve candidate Codex rollout files to the live processes that currently
/// hold them open. Codex keeps its active rollout descriptor open for the
/// lifetime of an interactive session, making this stronger than cwd/mtime
/// inference when several sessions run in one project.
pub fn open_codex_rollouts(pids: &[u32], candidates: &[PathBuf]) -> HashMap<u32, PathBuf> {
    use libproc::file_info::{pidfdinfo, ListFDs, PIDFDInfo, PIDFDInfoFlavor, ProcFDType};
    use libproc::proc_pid::{listpidinfo, pidinfo};
    use std::ffi::CStr;

    // Darwin's vnode_fdinfowithpath is:
    // proc_fileinfo (24) + vnode_info (152) + MAXPATHLEN (1024).
    // libproc exposes the generic pidfdinfo API but not this concrete binding,
    // so keep the unused ABI prefixes opaque and read only the path field.
    #[repr(C)]
    struct VnodeFdInfoWithPath {
        proc_fileinfo: [u8; 24],
        vnode_info: [u8; 152],
        path: [libc::c_char; 1024],
    }

    impl Default for VnodeFdInfoWithPath {
        fn default() -> Self {
            Self {
                proc_fileinfo: [0; 24],
                vnode_info: [0; 152],
                path: [0; 1024],
            }
        }
    }

    impl PIDFDInfo for VnodeFdInfoWithPath {
        fn flavor() -> PIDFDInfoFlavor {
            PIDFDInfoFlavor::VNodePathInfo
        }
    }

    debug_assert_eq!(std::mem::size_of::<VnodeFdInfoWithPath>(), 1200);
    debug_assert_eq!(std::mem::offset_of!(VnodeFdInfoWithPath, path), 176);

    let wanted: HashMap<PathBuf, &PathBuf> = candidates
        .iter()
        .map(|path| {
            (
                std::fs::canonicalize(path).unwrap_or_else(|_| path.clone()),
                path,
            )
        })
        .collect();
    let mut out = HashMap::new();
    for &pid in pids {
        let Ok(info) = pidinfo::<BSDInfo>(pid as i32, 0) else {
            continue;
        };
        let Ok(fds) = listpidinfo::<ListFDs>(pid as i32, info.pbi_nfiles as usize) else {
            continue;
        };
        for fd in fds {
            if !matches!(ProcFDType::from(fd.proc_fdtype), ProcFDType::VNode) {
                continue;
            }
            let Ok(vnode) = pidfdinfo::<VnodeFdInfoWithPath>(pid as i32, fd.proc_fd) else {
                continue;
            };
            let path = unsafe { CStr::from_ptr(vnode.path.as_ptr()) };
            let path = PathBuf::from(path.to_string_lossy().as_ref());
            if let Some(original) = wanted.get(&path) {
                out.insert(pid, (*original).clone());
                break;
            }
        }
    }
    out
}
