//! cc-hub-lib: session discovery, state, and TUI rendering for cc-hub. The
//! `cc-hub` binary owns the runtime and drives everything through this crate.
//! Start with `docs/codebase-tour.md` for the source layout.

pub mod acks;
pub mod agent;
pub mod agent_runtime;
pub mod app;
pub mod bookmarks;
pub mod builds;
pub mod clipboard;
pub mod config;
pub mod conversation;
pub mod focus;
pub mod folder_picker;
pub mod fuzzy;
pub mod gh;
pub mod harness;
pub mod link;
pub mod live_view;
pub mod metrics;
pub mod models;
pub mod ops;
pub mod persist;
pub mod platform;
pub mod resources;
pub mod respawn;
pub mod send;
pub mod sessions;
pub mod spawn;
pub mod tasks;
#[cfg(test)]
pub(crate) mod test_util;
pub mod title;
pub mod tmux_pane;
pub mod ui;
pub mod usage;
pub mod wake;
