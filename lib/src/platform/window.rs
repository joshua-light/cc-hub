//! Window-manager abstraction.
//!
//! Today we support Hyprland natively and X11 (via `xdotool`) as a fallback.
//! Detection runs once at first use and caches the selected chain in a
//! `OnceLock`. macOS and headless environments end up with an empty chain,
//! where every operation is a no-op.

use log::info;
use std::sync::OnceLock;

pub trait WindowManager: Send + Sync {
    fn name(&self) -> &'static str;

    /// Focus the window owning any pid in `pids`. Returns true on success.
    fn focus(&self, pids: &[u32]) -> bool;

    /// Close the window owning any pid in `pids` (graceful WM_DELETE /
    /// closewindow). Returns true on success.
    fn close(&self, pids: &[u32]) -> bool;
}

static CURRENT: OnceLock<Chain> = OnceLock::new();

/// Globally-cached WindowManager for the current host. Cheap to call.
pub fn current() -> &'static dyn WindowManager {
    CURRENT.get_or_init(detect)
}

fn detect() -> Chain {
    let mut managers: Vec<Box<dyn WindowManager>> = Vec::new();
    if hyprland::available() {
        managers.push(Box::new(hyprland::Hyprland));
    }
    if xdotool::available() {
        managers.push(Box::new(xdotool::Xdotool));
    }
    let names: Vec<&str> = managers.iter().map(|m| m.name()).collect();
    info!("window: detected managers = {:?}", names);
    Chain { managers }
}

/// Runs each underlying manager in order until one succeeds. Gives us the
/// "try Hyprland, fall back to xdotool" behaviour we already relied on.
struct Chain {
    managers: Vec<Box<dyn WindowManager>>,
}

impl WindowManager for Chain {
    fn name(&self) -> &'static str {
        "chain"
    }

    fn focus(&self, pids: &[u32]) -> bool {
        self.managers.iter().any(|m| m.focus(pids))
    }

    fn close(&self, pids: &[u32]) -> bool {
        self.managers.iter().any(|m| m.close(pids))
    }
}

mod hyprland {
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
}

mod xdotool {
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
}
