//! Host-OS coupling, one category per submodule, so the rest of the crate
//! stays generic.
//!
//! - [`mux`]: the tmux CLI, or psmux on Windows.
//! - [`paths`]: cache, config and agent data-dir paths.
//! - [`process`]: parent pid, name, liveness and agent detection per OS.
//! - [`terminal`]: launching a command in a terminal emulator.
//! - [`window`]: focusing and closing windows through the window manager.

pub mod mux;
pub mod paths;
pub mod process;
pub mod terminal;
pub mod window;
