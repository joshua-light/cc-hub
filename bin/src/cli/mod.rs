//! CLI subcommands.
//!
//! These run before the TUI starts up — when argv contains a known verb,
//! [`dispatch`] handles it and returns an exit code. The TUI in `main.rs`
//! never sees them.
//!
//! Argument parsing is hand-rolled to avoid a clap dep. Verbs: `board ...`
//! (the Tasks board), `open <url>` (a `cc-hub://` deep link; the OS
//! URL-scheme handler calls it), `agent ...`, `build ...`, `resource ...`, `usage` and
//! `wake`. They emit a single JSON line on stdout describing the result so a
//! calling agent can parse the outcome programmatically.

mod agent;
mod board;
mod build;
mod error;
mod flags;
mod help;
mod link;
mod resource;
mod usage;
mod wake;

use cc_hub_lib::ops;
use error::{handle, CliError};
use flags::{args_request_help, parse_flags, require_task};

pub fn dispatch(args: &[String]) -> Option<i32> {
    let (verb, rest) = args.split_first()?;
    if matches!(verb.as_str(), "help" | "--help" | "-h") {
        return Some(handle(help::print_cli_help(rest)));
    }
    if args_request_help(rest) {
        return Some(handle(help::print_cli_help(args)));
    }
    match verb.as_str() {
        "open" => Some(handle(link::open(rest))),
        "resource" => Some(handle(resource::resource(rest))),
        "agent" => Some(handle(agent::agent_subcommand(rest))),
        "board" => Some(handle(board::board_subcommand(rest))),
        "build" => Some(handle(build::build(rest))),
        "usage" => Some(handle(usage::usage(rest))),
        "wake" => Some(handle(wake::wake(rest))),
        _ => None,
    }
}

fn print_json(value: &serde_json::Value) {
    // One line per call so callers can split on \n. Pretty-print would
    // make Bash piping awkward.
    match serde_json::to_string(value) {
        Ok(s) => println!("{}", s),
        Err(e) => eprintln!("(failed to serialise output: {})", e),
    }
}

/// Map a [`PromptStatus`] to its JSON string, emitting the human warning line
/// to stderr for the `Deferred` case (presentation stays in cli.rs).
fn report_prompt_status(status: &ops::prompt::PromptStatus) -> &'static str {
    if let ops::prompt::PromptStatus::Deferred(warning) = status {
        eprintln!("warning: {}", warning);
    }
    status.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_handles_help() {
        assert_eq!(dispatch(&["--help".to_string()]), Some(0));
        assert_eq!(
            dispatch(&["board".to_string(), "--help".to_string()]),
            Some(0)
        );
    }
}
