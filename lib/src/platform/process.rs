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
    use log::debug;
    use std::path::Path;
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

/// Pure detector for `codex` CLI invocations from `comm` + `cmdline`. Both
/// inputs must be lowercased. Exact-basename match on the first cmdline
/// argument avoids substring false positives (e.g. a `codex-something` binary),
/// while still catching a Homebrew/npm shim path like `/opt/homebrew/bin/codex`.
/// Detect the `codex` CLI by the OS-reported executable name (comm on Linux,
/// the `proc_pidpath` basename on macOS, the image name on Windows) — NOT the
/// command line. A joined command line loses argv[0] boundaries, and several
/// ChatGPT-desktop-app binaries live under spaced paths (`Codex Framework
/// .framework`, `Codex Computer Use.app`) whose basename would mis-extract onto
/// "codex"; the OS name splits those correctly ("SkyComputerUseService", etc.).
fn matches_codex_command(name: &str) -> bool {
    name == "codex" || name == "codex.exe"
}

/// Codex subcommands that are NOT an interactive TUI session — background
/// servers, one-shot utilities, auth, etc. A `codex` process running one of
/// these is not a session the hub should surface (e.g. the ChatGPT desktop
/// app's `codex … app-server`, or `codex mcp-server`).
const CODEX_NON_INTERACTIVE: &[&str] = &[
    "exec",
    "e",
    "review",
    "login",
    "logout",
    "mcp",
    "mcp-server",
    "app-server",
    "app",
    "remote-control",
    "completion",
    "update",
    "doctor",
    "sandbox",
    "debug",
    "apply",
    "a",
    "archive",
    "delete",
    "unarchive",
    "cloud",
    "exec-server",
    "features",
    "help",
];

/// Codex options that consume the following token as their value, so the value
/// isn't mistaken for the subcommand. (`resume`/`fork` are interactive
/// subcommands and are deliberately absent from [`CODEX_NON_INTERACTIVE`].)
const CODEX_VALUE_FLAGS: &[&str] = &[
    "-c",
    "--config",
    "-m",
    "--model",
    "-s",
    "--sandbox",
    "-a",
    "--ask-for-approval",
    "--remote",
    "--enable",
    "--disable",
];

/// Whether a `codex` command line launches an interactive session (bare
/// `codex`, or `codex resume/fork …`) rather than a non-interactive subcommand.
/// Skips leading option/value pairs to find the first positional token.
///
/// `cmd` is expected lowercased (as [`is_agent_process`] passes it).
fn codex_is_interactive(cmd: &str) -> bool {
    // The ChatGPT desktop app bundles a "Codex Framework" (an Electron/Chromium
    // app) plus a background `app-server`. Its helper processes are Chromium
    // `--type=` / crashpad children whose spaced framework path
    // ("Codex Framework.framework") makes basename extraction misfire onto
    // "codex". None of them are CLI sessions.
    if cmd.contains("chatgpt.app")
        || cmd.contains("codex framework")
        || cmd.contains("--type=")
        || cmd.contains("crashpad")
    {
        return false;
    }
    let toks: Vec<&str> = cmd.split_whitespace().skip(1).collect();
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i];
        if t.starts_with('-') {
            if CODEX_VALUE_FLAGS.contains(&t) && !t.contains('=') {
                i += 2;
            } else {
                i += 1;
            }
        } else {
            return !CODEX_NON_INTERACTIVE.contains(&t);
        }
    }
    // No positional token → a bare interactive `codex`.
    true
}

/// The `-m` / `--model` argument of a live codex process, so a session with no
/// rollout on disk yet can still show which model it launched with.
pub fn codex_model_arg(pid: u32) -> Option<String> {
    codex_model_from_cmd(&command_line(pid))
}

fn codex_model_from_cmd(cmd: &str) -> Option<String> {
    let toks: Vec<&str> = cmd.split_whitespace().collect();
    for (i, t) in toks.iter().enumerate() {
        if let Some(v) = t.strip_prefix("--model=") {
            return Some(v.trim_matches(['\'', '"']).to_string());
        }
        if (*t == "-m" || *t == "--model") && i + 1 < toks.len() {
            return Some(toks[i + 1].trim_matches(['\'', '"']).to_string());
        }
    }
    None
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

#[cfg(target_os = "macos")]
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn command_line(_pid: u32) -> String {
    String::new()
}

#[cfg(target_os = "linux")]
pub fn current_dir(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{}/cwd", pid))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(target_os = "macos")]
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
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
    use libproc::processes::{pids_by_type, ProcFilter};
    pids_by_type(ProcFilter::All).unwrap_or_default()
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

    #[test]
    fn codex_detector_matches_exe_name_only() {
        // Matches on the OS-reported executable name, not the command line.
        assert!(matches_codex_command("codex"));
        assert!(matches_codex_command("codex.exe"));
        // Spaced-path desktop-app binaries resolve to their real basenames and
        // must NOT match (the bug the cmdline heuristic caused).
        assert!(!matches_codex_command("skycomputeruseservice"));
        assert!(!matches_codex_command("codex (service)"));
        assert!(!matches_codex_command("browser_crashpad_handler"));
        assert!(!matches_codex_command("codexer"));
    }

    #[test]
    fn codex_interactive_accepts_bare_and_resume() {
        // Bare interactive launch with only option/value pairs.
        assert!(codex_is_interactive(
            "/opt/homebrew/bin/codex -c model_reasoning_effort=high -m gpt-5.6-luna"
        ));
        // resume / fork produce interactive sessions.
        assert!(codex_is_interactive("codex resume 019f-uuid"));
        assert!(codex_is_interactive("codex fork --last"));
        // A trailing positional prompt is still interactive.
        assert!(codex_is_interactive("codex -m gpt-5.6-luna hello world"));
    }

    #[test]
    fn codex_interactive_rejects_servers_and_oneshots() {
        // The ChatGPT desktop app's background server.
        assert!(!codex_is_interactive(
            "/Applications/ChatGPT.app/Contents/Resources/codex -c features.code_mode_host=true app-server --analytics-default-enabled"
        ));
        assert!(!codex_is_interactive("codex mcp-server"));
        assert!(!codex_is_interactive("codex exec 'do a thing'"));
        // `-s sandbox` is a value, not the `sandbox` subcommand → still a
        // bare interactive launch.
        assert!(codex_is_interactive("codex -s sandbox -m gpt-5.6-luna"));
        // `codex sandbox <cmd>` as a subcommand is non-interactive.
        assert!(!codex_is_interactive("codex sandbox ls"));
    }

    #[test]
    fn codex_interactive_rejects_chatgpt_desktop_app_helpers() {
        // Inputs are lowercased, as is_agent_process passes them. A spaced
        // framework path would otherwise mis-extract basename "codex".
        assert!(!codex_is_interactive(
            "/applications/chatgpt.app/contents/frameworks/codex framework.framework/versions/150/helpers/browser_crashpad_handler --monitor-self"
        ));
        assert!(!codex_is_interactive(
            "/applications/chatgpt.app/contents/frameworks/codex framework.framework/versions/150/helpers/codex (service).app/contents/macos/codex (service) --type=gpu-process"
        ));
    }

    #[test]
    fn codex_model_arg_parsing() {
        assert_eq!(
            codex_model_from_cmd("/x/codex -c a=b -m gpt-5.6-luna").as_deref(),
            Some("gpt-5.6-luna")
        );
        assert_eq!(
            codex_model_from_cmd("codex --model='o3'").as_deref(),
            Some("o3")
        );
        assert_eq!(codex_model_from_cmd("codex resume 019f-uuid"), None);
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
