//! CLI subcommands. When argv starts with a known verb, [`dispatch`] runs it
//! before the TUI starts and returns the exit code. Parsing is hand-rolled
//! to avoid a clap dep. Each verb prints one JSON line on stdout so a calling
//! agent can parse the outcome.
//!
//! - `error`: [`CliError`] and the JSON error contract.
//! - `flags`: the shared `--flag` parser and the help-request scan.
//! - `board`, `link` (`open`), `agent`, `build`, `resource`, `usage`,
//!   `wake`: one file per verb.
//! - `help`: `--help` text.

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

/// Map a [`PromptStatus`](ops::prompt::PromptStatus) to its JSON string,
/// printing the human warning to stderr for `Deferred`. Presentation stays
/// in the CLI, not in `ops`.
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
