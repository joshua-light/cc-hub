//! Personal task board shown on the Tasks tab. A board task is a plain to-do
//! item that can optionally be handed to a single agent session: assigning spawns a detached agent in a
//! chosen cwd, prompted to investigate and plan first — the card sits in
//! Planning until the user approves the plan (Space), which tells the agent
//! to proceed and moves the card to In Progress. The binding is recorded so
//! `f` on the card attaches to that session exactly like the Sessions tab.
//! When that session opens a pull request it writes a `PR:` note, and the
//! note carries the card to Review — the column for work that wants reading
//! rather than answering.
//!
//! A board task is a [`store::TaskState`], stored one file per task at
//! `~/.cc-hub/tasks/<task-id>/state.json` behind a per-task lock, with the
//! legal status edges enforced by the store's transition table. [`PersonalBoard`]
//! is the in-memory snapshot the TUI mutates through; every mutation is a
//! locked read-mutate-write of the task's own file, so concurrent cc-hub
//! instances conflict per task, not per board. Board-level metadata
//! (`last_assign_cwd`) lives in `~/.cc-hub/board.json`.
//!
//! - [`store`] — per-task `state.json` files and the status transition table.
//! - [`activity`] — the card's progress label, from the task's notes and side files.
//! - [`stats`] — persisted per-task usage.
//! - [`session_links`] — user-driven session→task links.

mod archive;
mod binding;
mod board;
mod meta;
mod quick_add;
mod status;

pub mod activity;
pub mod session_links;
pub mod stats;
pub mod store;

pub use board::PersonalBoard;
pub use quick_add::{parse_quick_add, parse_tags, QuickAdd};
