//! Hyprland: `hyprctl` over the compositor's IPC.

use super::WindowManager;
use log::{debug, info, warn};
use std::process::Command;

pub struct Hyprland;

pub fn available() -> bool {
    // Hyprland exports this to every client; avoids paying for a hyprctl
    // spawn just to probe. If it's set but hyprctl is broken, individual
    // calls still fail gracefully and the chain falls through.
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

/// Fetch Hyprland clients and return the first `(pid, client_value)` whose
/// pid matches one in `pids`.
fn find_client(pids: &[u32]) -> Option<(u32, serde_json::Value)> {
    let output = Command::new("hyprctl")
        .args(["clients", "-j"])
        .output()
        .ok()?;
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

fn dispatch(command: &str, pids: &[u32]) -> bool {
    let Some((p, _)) = find_client(pids) else {
        debug!("no ancestor PID matched a hyprland client");
        return false;
    };
    let addr = format!("pid:{}", p);
    info!("hyprctl: {} pid {}", command, p);
    match Command::new("hyprctl")
        .args(["dispatch", command, &addr])
        .output()
    {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            info!(
                "  hyprctl dispatch {} status={}, stdout={:?}, stderr={:?}",
                command,
                out.status,
                stdout.trim(),
                stderr.trim()
            );
            out.status.success()
        }
        Err(e) => {
            warn!("  hyprctl dispatch {} failed: {}", command, e);
            false
        }
    }
}

impl WindowManager for Hyprland {
    fn name(&self) -> &'static str {
        "hyprland"
    }

    fn focus(&self, pids: &[u32]) -> bool {
        dispatch("focuswindow", pids)
    }

    fn close(&self, pids: &[u32]) -> bool {
        dispatch("closewindow", pids)
    }
}
