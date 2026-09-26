//! X11 fallback: `xdotool` looks windows up by pid.

use super::WindowManager;
use log::{debug, info, warn};
use std::process::Command;

pub struct Xdotool;

pub fn available() -> bool {
    // Pay the probe cost once at startup. The result is cached by the
    // top-level OnceLock, so subsequent calls don't re-exec `command -v`.
    Command::new("sh")
        .args(["-c", "command -v xdotool >/dev/null 2>&1"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn act(pids: &[u32], action: &str) -> bool {
    for p in pids {
        let output = Command::new("xdotool")
            .args(["search", "--pid", &p.to_string()])
            .output();

        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if let Some(window_id) = stdout.lines().next().filter(|s| !s.is_empty()) {
                    info!("found window {} for pid {}, {}", window_id, p, action);
                    let result = Command::new("xdotool").args([action, window_id]).output();
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

impl WindowManager for Xdotool {
    fn name(&self) -> &'static str {
        "xdotool"
    }

    fn focus(&self, pids: &[u32]) -> bool {
        act(pids, "windowactivate")
    }

    fn close(&self, pids: &[u32]) -> bool {
        act(pids, "windowclose")
    }
}
