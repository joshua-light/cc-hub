use log::{debug, info, warn};
use std::fs;
use std::process::Command;

/// Focus the terminal window that contains the given process.
///
/// Walks up the process tree from `pid` to collect ancestor PIDs,
/// then tries Hyprland (hyprctl) first, falling back to X11 (xdotool).
/// Walk the process tree upward from `start`, appending ancestor PIDs.
fn walk_ancestors(pids: &mut Vec<u32>, start: u32, label: &str) {
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

    info!("process chain: {:?}", pids);

    if try_hyprland(&pids) {
        return;
    }

    if try_xdotool(&pids) {
        return;
    }

    warn!("no window found for pid {} or any ancestor", pid);
}

/// Try focusing via Hyprland IPC (hyprctl dispatch focuswindow pid:N).
fn try_hyprland(pids: &[u32]) -> bool {
    // Get list of known client PIDs from Hyprland
    let clients_output = match Command::new("hyprctl").args(["clients", "-j"]).output() {
        Ok(out) => out,
        Err(e) => {
            debug!("hyprctl not available: {}", e);
            return false;
        }
    };

    let clients_json = String::from_utf8_lossy(&clients_output.stdout);
    debug!("hyprctl clients returned {} bytes", clients_json.len());

    // Parse client PIDs from JSON array
    let client_pids: Vec<u32> = serde_json::from_str::<Vec<serde_json::Value>>(&clients_json)
        .unwrap_or_default()
        .iter()
        .filter_map(|c| c.get("pid")?.as_u64().map(|p| p as u32))
        .collect();

    debug!("hyprland client pids: {:?}", client_pids);

    // Find the first ancestor PID that matches a Hyprland client
    for p in pids {
        if client_pids.contains(p) {
            let addr = format!("pid:{}", p);
            info!("hyprctl: focusing window with pid {}", p);
            let result = Command::new("hyprctl")
                .args(["dispatch", "focuswindow", &addr])
                .output();
            match result {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    info!(
                        "  hyprctl dispatch status={}, stdout={:?}, stderr={:?}",
                        out.status,
                        stdout.trim(),
                        stderr.trim()
                    );
                    return out.status.success();
                }
                Err(e) => {
                    warn!("  hyprctl dispatch failed: {}", e);
                    return false;
                }
            }
        }
    }

    debug!("no ancestor PID matched a hyprland client");
    false
}

/// Try focusing via X11 xdotool (fallback for X11/XWayland).
fn try_xdotool(pids: &[u32]) -> bool {
    for p in pids {
        debug!("trying xdotool search --pid {}", p);
        let output = Command::new("xdotool")
            .args(["search", "--pid", &p.to_string()])
            .output();

        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                debug!(
                    "  xdotool search --pid {}: status={}, stdout={:?}, stderr={:?}",
                    p, out.status, stdout.trim(), stderr.trim()
                );

                if let Some(window_id) = stdout.lines().next().filter(|s| !s.is_empty()) {
                    info!("found window {} for pid {}, activating", window_id, p);
                    let activate = Command::new("xdotool")
                        .args(["windowactivate", window_id])
                        .output();
                    match activate {
                        Ok(a) => {
                            let astderr = String::from_utf8_lossy(&a.stderr);
                            info!(
                                "  windowactivate status={}, stderr={:?}",
                                a.status,
                                astderr.trim()
                            );
                        }
                        Err(e) => warn!("  windowactivate failed to spawn: {}", e),
                    }
                    return true;
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
