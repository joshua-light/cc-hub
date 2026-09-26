//! Domain logic for compound operations on tasks and deep links, shared by
//! the CLI (`bin/src/cli/`) and the TUI (`lib/src/app/`). The CLI keeps
//! argument parsing, JSON rendering and exit-code mapping; everything that
//! mutates on-disk state lives here.
//!
//! Conventions:
//!   * Ops take typed parameters, grouped into small option structs for
//!     many-arg verbs, never the CLI's `Flags`.
//!   * Ops return typed results (the `TaskState` they produce, or an outcome
//!     enum when a verb has several result shapes). The caller renders its
//!     JSON or human output from them.
//!   * Presentation side effects (`println!`, `print_json`, `eprintln!`)
//!     stay in the caller. `log::*` diagnostics may live here.
//!   * Ops mutate tasks through `store::update_task`, which holds the
//!     per-task lock and validates transitions.
//!
//! - `link`: act on a parsed `cc-hub://` link (`cc-hub open`).
//! - `prompt`: deliver a prompt once a fresh session is ready for input.
//! - `task`: attach, list and remove a card's notes and attachments.

pub mod link;
pub mod prompt;
pub mod task;

/// Error type for domain ops. Mirrors the `CliError` variants domain code
/// needs, so the CLI converts losslessly through its `From<OpError>` impl.
#[derive(Debug)]
pub enum OpError {
    /// Bad invocation: missing/unknown flag, malformed value, illegal
    /// transition the caller could have avoided. Maps to `CliError::Usage`
    /// (exit 2, kind "usage").
    Usage(String),
    /// Requested entity does not exist. Maps to `CliError::NotFound`
    /// (exit 1, kind "notfound").
    NotFound(String),
    /// State guard tripped. Carries an optional remediation recipe. Maps to
    /// `CliError::Conflict` (exit 1, kind "conflict").
    Conflict { msg: String, recipe: Option<String> },
    /// Everything else (I/O, serialization). Maps to
    /// `CliError::Other` (exit 1, kind "other").
    Other(String),
}

impl OpError {
    /// A `conflict` error carrying a remediation recipe for the caller.
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
            OpError::Usage(m) | OpError::NotFound(m) | OpError::Other(m) => f.write_str(m),
            OpError::Conflict { msg, .. } => f.write_str(msg),
        }
    }
}
