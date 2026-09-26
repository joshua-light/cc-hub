//! The personal task board behind the Tasks tab.
//!
//! Each card is a [`store::TaskState`] in `~/.cc-hub/tasks/<task-id>/state.json`.
//! Every mutation is a read-mutate-write under that task's own lock, so
//! concurrent cc-hub instances conflict per task, not per board.
//! [`PersonalBoard`] is the in-memory snapshot the TUI mutates through.
//!
//! - `board`: [`PersonalBoard`], the loaded board and its mutations.
//! - `binding`: keeps a card's `session_id` and `tmux` in step with the scan.
//! - `quick_add`: the add popup's `#tag` / `!N` syntax and tag normalization.
//! - `archive`: the append-only log of removed cards.
//! - `meta`: board-level state in `~/.cc-hub/board.json`.
//! - [`store`]: per-task `state.json` files, locking and atomic writes.
//! - `status`: card columns, priorities and the legal-transition table.
//! - [`activity`]: the card's progress label, from its notes and side files.
//! - [`stats`]: per-card token and cost totals.
//! - [`session_links`]: user-driven session→task links.

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
