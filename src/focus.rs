use log::{debug, info, warn};
use std::fs;
use std::process::Command;

/// Focus the terminal window that contains the given process.
///
/// Walks up the process tree from `pid` to collect ancestor PIDs,
/// then tries Hyprland (hyprctl) first, falling back to X11 (xdotool).
/// Walk the process tree upward from `start`, appending ancestor PIDs.
pub(crate) fn walk_ancestors(pids: &mut Vec<u32>, start: u32, label: &str) {
    let mut current = start;
    while let Some(ppid) = parent_pid(current) {
        if ppid <= 1 {
            debug!("reached init (ppid={}), stopping {} walk", ppid, label);
            break;
        }
        let comm = proc_comm(ppid);
        debug!("  {} {} -> parent {} ({})", label, current, ppid, comm);
        pids.push(ppid);
        current = ppid;
    }
}

pub fn focus_window(pid: u32) {
    info!("focus_window called for pid={}", pid);
    let pids = collect_pid_chain(pid);
    info!("process chain: {:?}", pids);
    if !act_on_window(&pids, "focuswindow", "windowactivate") {
        warn!("no window found for pid {} or any ancestor", pid);
    }
}

/// Ask the terminal window owning `pid` to close. Returns true on success.
///
/// On Hyprland, dispatches `closewindow`, which sends a close request to the
/// terminal — the terminal then exits, sending SIGHUP to its foreground
/// process group (killing the Claude session inside). On X11, sends a
/// graceful WM_DELETE_WINDOW via `xdotool windowclose`.
pub fn close_window(pid: u32) -> bool {
    info!("close_window called for pid={}", pid);
    let pids = collect_pid_chain(pid);
    info!("process chain: {:?}", pids);
    let ok = act_on_window(&pids, "closewindow", "windowclose");
    if !ok {
        warn!("no window found to close for pid {} or any ancestor", pid);
    }
    ok
}

fn act_on_window(pids: &[u32], hypr_dispatch: &str, xdotool_action: &str) -> bool {
    try_hyprland(pids, hypr_dispatch) || try_xdotool(pids, xdotool_action)
}

fn collect_pid_chain(pid: u32) -> Vec<u32> {
    let mut pids = vec![pid];
    walk_ancestors(&mut pids, pid, "pid");

    // If the walk was cut short (process reparented to init), fall back to
    // the session leader: all processes in a terminal session share the same
    // session ID, and the session leader's parent is typically the terminal
    // emulator window we want to focus.
    if pids.len() <= 1 {
        if let Some(sid) = proc_session_id(pid) {
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

/// Fetch Hyprland clients and return the first `(pid, client_value)` whose
/// pid matches one in `pids`. Returns None if hyprctl isn't reachable or no
/// match exists.
fn find_hypr_client(pids: &[u32]) -> Option<(u32, serde_json::Value)> {
    let output = Command::new("hyprctl").args(["clients", "-j"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let clients: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).ok()?;
    for p in pids {
        for client in &clients {
            let cpid = client.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            if cpid == *p {
                return Some((cpid, client.clone()));
            }
        }
    }
    None
}

fn try_hyprland(pids: &[u32], dispatch: &str) -> bool {
    let Some((p, _)) = find_hypr_client(pids) else {
        debug!("no ancestor PID matched a hyprland client");
        return false;
    };
    let addr = format!("pid:{}", p);
    info!("hyprctl: {} pid {}", dispatch, p);
    match Command::new("hyprctl")
        .args(["dispatch", dispatch, &addr])
        .output()
    {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            info!(
                "  hyprctl dispatch {} status={}, stdout={:?}, stderr={:?}",
                dispatch,
                out.status,
                stdout.trim(),
                stderr.trim()
            );
            out.status.success()
        }
        Err(e) => {
            warn!("  hyprctl dispatch {} failed: {}", dispatch, e);
            false
        }
    }
}

fn try_xdotool(pids: &[u32], action: &str) -> bool {
    for p in pids {
        let output = Command::new("xdotool")
            .args(["search", "--pid", &p.to_string()])
            .output();

        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if let Some(window_id) = stdout.lines().next().filter(|s| !s.is_empty()) {
                    info!("found window {} for pid {}, {}", window_id, p, action);
                    let result = Command::new("xdotool")
                        .args([action, window_id])
                        .output();
                    match result {
                        Ok(a) => {
                            let astderr = String::from_utf8_lossy(&a.stderr);
                            info!(
                                "  {} status={}, stderr={:?}",
                                action,
                                a.status,
                                astderr.trim()
                            );
                            return a.status.success();
                        }
                        Err(e) => {
                            warn!("  {} failed to spawn: {}", action, e);
                            return false;
                        }
                    }
                }
            }
            Err(e) => {
                debug!("  xdotool not available: {}", e);
                return false;
            }
        }
    }
    false
}

pub(crate) fn stat_fields(pid: u32) -> Option<Vec<String>> {
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

pub(crate) fn parent_pid(pid: u32) -> Option<u32> {
    // fields[1] = ppid
    stat_fields(pid)?.get(1)?.parse().ok()
}

fn proc_session_id(pid: u32) -> Option<u32> {
    // fields[3] = session
    stat_fields(pid)?.get(3)?.parse().ok()
}

pub(crate) fn proc_comm(pid: u32) -> String {
    fs::read_to_string(format!("/proc/{}/comm", pid))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Find the Hyprland workspace of the window owning `pid` (or any ancestor).
/// Returns None if hyprctl isn't available or no matching client exists.
pub fn workspace_for_pid(pid: u32) -> Option<i64> {
    let mut pids = vec![pid];
    walk_ancestors(&mut pids, pid, "ws");

    let (cpid, client) = find_hypr_client(&pids)?;
    let ws = client.get("workspace")?.get("id")?.as_i64()?;
    debug!("pid {} -> client pid {} on workspace {}", pid, cpid, ws);
    Some(ws)
}
