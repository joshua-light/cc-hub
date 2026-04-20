//! Platform abstractions.
//!
//! Each submodule hides a different category of host-OS coupling so the rest
//! of the codebase can stay generic:
//!
//! - [`process`]: parent pid / process name / session id / liveness — split by
//!   `#[cfg(target_os = ...)]` into a Linux procfs path and a macOS libproc
//!   path, both implementing [`process::ProcessInfo`].
//! - [`window`]: window-manager operations (focus/close/workspace). Detection
//!   is runtime: Hyprland when `HYPRLAND_INSTANCE_SIGNATURE` is set, X11 via
//!   `xdotool` as a fallback, otherwise a no-op impl.
//! - [`terminal`]: terminal-emulator launching (kitty/alacritty/foot/wezterm/
//!   ghostty). Each emulator plugs in as a [`terminal::Launcher`] so adding a
//!   new one is a single struct.
//! - [`paths`]: XDG-aware cache/config paths so we don't bake `/tmp` or
//!   `~/.config/hypr` into unrelated code.

pub mod mux;
pub mod paths;
pub mod process;
pub mod terminal;
pub mod window;
