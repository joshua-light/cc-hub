//! Shared domain logic for the orchestrator layer's compound operations.
//!
//! These functions are the single implementation of the task/PR/worker state
//! transitions that both the CLI (`bin/src/cli/`) and the TUI
//! (`lib/src/app/`) drive. The CLI keeps argument parsing, JSON rendering,
//! and exit-code mapping; everything that mutates on-disk state lives here.
//!
//! Conventions:
//!   * Ops take explicit typed parameters (`project_id: &str`, …), grouped
//!     into small option structs for many-arg verbs — never the CLI's
//!     `Flags`.
//!   * Ops return typed results (the `TaskState` / `PullRequest` they produce,
//!     or a small outcome enum when a verb has multiple result shapes). The
//!     caller reconstructs its JSON / human output from the returned data.
//!   * Presentation side effects (`println!`, `print_json`, `eprintln!`
//!     warnings) stay in the caller. `log::*` diagnostics may live here.
//!   * Ops keep using `orchestrator::update_task_state` / `pr::update_pr` /
//!     `merge_lock::*` — the per-task lock and transition validation live
//!     inside those helpers.

pub mod link;
pub mod pr;
pub mod task;
pub mod worker;

/// Error type for domain ops. Mirrors the variants of the CLI's
/// `CliError` that domain code needs, so the CLI can convert losslessly via a
/// `From<OpError>` impl on its side (and tests asserting `kind()` /
/// `CliError::Usage(..)` keep passing).
#[derive(Debug)]
pub enum OpError {
    /// Bad invocation: missing/unknown flag, malformed value, illegal
    /// transition the caller could have avoided. Maps to `CliError::Usage`
    /// (exit 2, kind "usage").
    Usage(String),
    /// Requested entity does not exist (no task / no PR). Maps to
    /// `CliError::NotFound` (exit 1, kind "notfound").
    NotFound(String),
    /// State guard tripped: conflicting transition on a terminal PR, etc.
    /// Carries an optional remediation recipe. Maps to `CliError::Conflict`
    /// (exit 1, kind "conflict").
    Conflict { msg: String, recipe: Option<String> },
    /// Everything else (I/O, git failures, serialization). Maps to
    /// `CliError::Other` (exit 1, kind "other").
    Other(String),
    /// The op already produced a rich, domain-specific result the caller
    /// printed as its own `{"ok":false,...}` JSON line. The caller must NOT
    /// print a second JSON line; it only sets the nonzero exit code and a
    /// human stderr line. The string is that stderr message. Maps to
    /// `CliError::Reported`.
    Reported(String),
}

impl OpError {
    /// A `conflict` error carrying a remediation recipe for the orchestrator.
    pub fn conflict_with_recipe(msg: impl Into<String>, recipe: impl Into<String>) -> Self {
        OpError::Conflict {
            msg: msg.into(),
            recipe: Some(recipe.into()),
        }
    }
}

/// Human-readable message — the TUI surfaces this in its status bar. The
/// CLI does not use it (it maps variants onto `CliError` instead).
impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpError::Usage(m) | OpError::NotFound(m) | OpError::Other(m) | OpError::Reported(m) => {
                f.write_str(m)
            }
            OpError::Conflict { msg, .. } => f.write_str(msg),
        }
    }
}
