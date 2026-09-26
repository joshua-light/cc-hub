//! Session discovery: find every agent session on disk and report its state.
//! Read-only: nothing here writes to an agent's transcript or status files.
//!
//! - [`scanner`] — the live-grid scan; merges Claude, Pi and Codex sessions.
//! - `claude` — Claude session discovery.
//! - [`pi`] — Pi session discovery.
//! - [`codex`] — Codex session discovery.
//! - `common` — helpers shared by the three scanners.
//! - [`pi_bridge`] — Pi extension heartbeat files.
//! - [`dir_cache`] — per-directory listing cache for the orphan/inactive walks.
//! - [`index`] — the session archive: every transcript on disk, however old.
//! - [`count`] — sessions created today and this week.
//! - [`watcher`] — filesystem watcher that signals a rescan.

mod claude;
pub mod codex;
mod common;
pub mod count;
pub mod dir_cache;
pub mod index;
pub mod pi;
pub mod pi_bridge;
pub mod scanner;
pub mod watcher;
