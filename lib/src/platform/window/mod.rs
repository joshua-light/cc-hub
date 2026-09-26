//! Window-manager abstraction: focus or close the window owning a pid.
//!
//! Detection runs once at first use and caches the chain of available
//! backends in a `OnceLock`; each operation tries them in order. Headless
//! environments get an empty chain, where every operation is a no-op.
//!
//! - [`hyprland`]: Hyprland via `hyprctl`, when `HYPRLAND_INSTANCE_SIGNATURE`
//!   is set.
//! - [`xdotool`]: X11 via `xdotool`, when it is installed.
//! - `macos`: CoreGraphics finds the window, the Accessibility API raises it
//!   and AppleScript activates its app. The first AX call triggers macOS's
//!   Accessibility permission prompt; until the user grants it, focus and
//!   close fail and the caller sees the usual "no window" status.

use log::info;
use std::sync::OnceLock;

mod hyprland;
#[cfg(target_os = "macos")]
mod macos;
mod xdotool;

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
    #[cfg(target_os = "macos")]
    if macos::available() {
        managers.push(Box::new(macos::Macos));
    }
    let names: Vec<&str> = managers.iter().map(|m| m.name()).collect();
    info!("window: detected managers = {:?}", names);
    Chain { managers }
}

/// Runs each underlying manager in order until one succeeds, so Hyprland
/// falls back to xdotool.
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
