//! CLI subcommands.
//!
//! These run before the TUI starts up — when argv contains a known verb,
//! [`dispatch`] handles it and returns an exit code. The TUI in `main.rs`
//! never sees them.
//!
//! Argument parsing is hand-rolled to avoid a clap dep. Verbs: `board ...`
//! (the Tasks board), `open <url>` (a `cc-hub://` deep link; the OS
//! URL-scheme handler calls it), `agent ...`, `resource ...`, `usage` and
//! `wake`. They emit a single JSON line on stdout describing the result so a
//! calling agent can parse the outcome programmatically.

mod agent;
mod board;
mod help;
mod link;
mod resource;
#[cfg(test)]
mod test_util;
mod usage;
mod wake;

use cc_hub_lib::ops::{self, OpError};

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
        "usage" => Some(handle(usage::usage(rest))),
        "wake" => Some(handle(wake::wake(rest))),
        _ => None,
    }
}

/// Terminate a verb by emitting the one-JSON-line contract and returning the
/// process exit code.
///
/// The module contract is "one JSON line on stdout". On success the verb
/// already printed its own JSON; on error we print a structured error line
/// here — `{"ok":false,"error":"<msg>","kind":"<usage|notfound|conflict|
/// other>"}` (plus `recipe` when the variant carries a hint) — so a caller
/// piping stdout to `jq` always sees a parseable result, even
/// on the failures that matter most. A short human line still goes to
/// stderr for interactive use.
fn handle(result: Result<(), CliError>) -> i32 {
    match result {
        Ok(()) => 0,
        // The verb already printed a richer `{"ok":false,...}` line carrying
        // domain fields the generic shape can't express. Don't double-print
        // JSON — just emit a human line and the exit code.
        Err(CliError::Reported(msg)) => {
            eprintln!("error: {}", msg);
            1
        }
        Err(err) => {
            let kind = err.kind();
            let (msg, recipe) = err.into_message_and_recipe();
            let mut payload = serde_json::json!({
                "ok": false,
                "error": msg,
                "kind": kind,
            });
            if let Some(recipe) = recipe.as_deref() {
                payload["recipe"] = serde_json::Value::String(recipe.to_string());
            }
            print_json(&payload);
            eprintln!("{} error: {}", kind, msg);
            match kind {
                "usage" => 2,
                _ => 1,
            }
        }
    }
}

#[derive(Debug)]
enum CliError {
    /// Bad invocation: missing/unknown flag, malformed value, illegal
    /// transition the caller could have avoided. Exit 2, kind "usage".
    Usage(String),
    /// Requested entity does not exist. Exit 1, kind "notfound".
    NotFound(String),
    /// State guard tripped. Exit 1, kind "conflict". Carries an optional
    /// recipe the caller can act on.
    Conflict { msg: String, recipe: Option<String> },
    /// Everything else (I/O, serialization). Exit 1,
    /// kind "other".
    Other(String),
    /// The verb already printed its own `{"ok":false,...}` JSON line (a rich,
    /// domain-specific payload). `handle` must NOT print a second JSON line;
    /// it only sets the nonzero exit code and a human stderr line. The string
    /// is that stderr message.
    Reported(String),
}

impl CliError {
    /// Stable machine-readable category for the JSON error contract.
    fn kind(&self) -> &'static str {
        match self {
            CliError::Usage(_) => "usage",
            CliError::NotFound(_) => "notfound",
            CliError::Conflict { .. } => "conflict",
            CliError::Other(_) => "other",
            // Never surfaced as a `kind` (handled before this is consulted),
            // but map it for completeness.
            CliError::Reported(_) => "other",
        }
    }

    /// Decompose into the human message and an optional remediation recipe.
    fn into_message_and_recipe(self) -> (String, Option<String>) {
        match self {
            CliError::Usage(msg)
            | CliError::NotFound(msg)
            | CliError::Other(msg)
            | CliError::Reported(msg) => (msg, None),
            CliError::Conflict { msg, recipe } => (msg, recipe),
        }
    }
}

impl From<String> for CliError {
    fn from(s: String) -> Self {
        CliError::Other(s)
    }
}

impl From<OpError> for CliError {
    /// Lossless 1:1 mapping from the domain-layer error to the CLI error so
    /// the JSON error contract (kind / recipe / exit code) is unchanged.
    fn from(e: OpError) -> Self {
        match e {
            OpError::Usage(msg) => CliError::Usage(msg),
            OpError::NotFound(msg) => CliError::NotFound(msg),
            OpError::Conflict { msg, recipe } => CliError::Conflict { msg, recipe },
            OpError::Other(msg) => CliError::Other(msg),
            OpError::Reported(msg) => CliError::Reported(msg),
        }
    }
}

impl<E: std::fmt::Display> From<(&'static str, E)> for CliError {
    fn from((ctx, e): (&'static str, E)) -> Self {
        CliError::Other(format!("{}: {}", ctx, e))
    }
}

#[derive(Default)]
struct Flags {
    task: Option<String>,
    agent: Option<String>,
    kind: Option<String>,
    wait_secs: Option<u64>,
    dry_run: bool,
    /// `board add` flags: the card's text, its tags (one string, split like
    /// the TUI's quick-capture field) and its priority (`p1`..`p4`).
    text: Option<String>,
    tags: Option<String>,
    priority: Option<String>,
    title: Option<String>,
    json: bool,
}

fn parse_flags(args: &[String]) -> Result<Flags, CliError> {
    let mut f = Flags::default();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--task" => {
                f.task = Some(next_value(args, &mut i, "--task")?);
            }
            "--agent" => {
                f.agent = Some(next_value(args, &mut i, "--agent")?);
            }
            "--kind" => {
                f.kind = Some(next_value(args, &mut i, "--kind")?);
            }
            "--wait-secs" => {
                let v = next_value(args, &mut i, "--wait-secs")?;
                f.wait_secs = Some(
                    v.parse()
                        .map_err(|e| CliError::Usage(format!("--wait-secs: {}", e)))?,
                );
            }
            "--dry-run" => {
                f.dry_run = true;
                i += 1;
            }
            "--title" => {
                f.title = Some(next_value(args, &mut i, "--title")?);
            }
            "--text" => {
                f.text = Some(next_value(args, &mut i, "--text")?);
            }
            "--tags" => {
                f.tags = Some(next_value(args, &mut i, "--tags")?);
            }
            "--priority" => {
                f.priority = Some(next_value(args, &mut i, "--priority")?);
            }
            "--json" => {
                f.json = true;
                i += 1;
            }
            other => {
                return Err(CliError::Usage(format!("unknown flag: {}", other)));
            }
        }
    }
    Ok(f)
}

/// Free-text flags whose value may legitimately begin with `--`
/// (e.g. `--text "--wip notes"`). For these we accept the next token
/// verbatim.
const FREE_TEXT_FLAGS: &[&str] = &["--title", "--text"];

fn next_value(args: &[String], i: &mut usize, name: &str) -> Result<String, CliError> {
    *i += 1;
    // For STRUCTURED flags (ids, enums, numbers, paths), a value that looks like
    // another flag means the caller omitted the value (e.g. `--task --json`
    // would otherwise bind task="--json") — reject it. Free-text
    // flags may legitimately take a `--`-prefixed value, so don't filter those.
    let reject_flag_like = !FREE_TEXT_FLAGS.contains(&name);
    let Some(v) = args
        .get(*i)
        .cloned()
        .filter(|v| !(reject_flag_like && v.starts_with("--")))
    else {
        return Err(CliError::Usage(format!("{} requires a value", name)));
    };
    *i += 1;
    Ok(v)
}

/// Boolean flags that take no value — the no-argument arms of [`parse_flags`].
/// Every other recognized `--flag` consumes the following token as its value.
const BOOL_FLAGS: &[&str] = &["--dry-run", "--json"];

/// Flag-aware scan for a help request among a verb's args.
///
/// A naive `args.iter().any(|a| a == "-h" || a == "--help")` misfires when a
/// flag VALUE equals `-h`/`--help` — e.g. `board note --text "-h"` would
/// silently reroute to help and exit 0, which the caller reads as a success
/// no-op. So mirror [`next_value`]'s tokenizer: a value-consuming flag
/// swallows its next token (free-text flags take any token; structured flags
/// reject a `--`-prefixed one, treating it as an omitted value). Only a
/// `-h`/`--help` in true argument position triggers help.
fn args_request_help(args: &[String]) -> bool {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if matches!(a, "-h" | "--help") {
            return true;
        }
        // Any `--flag` that isn't a known boolean — including flags this scan
        // doesn't recognize — swallows the following token as its value, so a
        // `-h`-shaped value can't masquerade as a help request. (For unknown
        // flags that means `--bogus -h` surfaces the unknown-flag usage error
        // rather than help — the more informative failure.)
        if a.starts_with("--") && !BOOL_FLAGS.contains(&a) {
            if let Some(next) = args.get(i + 1) {
                if FREE_TEXT_FLAGS.contains(&a) || !next.starts_with("--") {
                    i += 2;
                    continue;
                }
            }
        }
        i += 1;
    }
    false
}

fn require_task(f: &Flags) -> Result<String, CliError> {
    f.task
        .clone()
        .ok_or_else(|| CliError::Usage("--task is required".into()))
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

    #[test]
    fn help_scan_ignores_free_text_flag_value() {
        // Regression: `board note --text "-h"` — the `-h` is the note VALUE,
        // not a help request. Rerouting to help here is a silent exit-0 no-op
        // the caller misreads as success.
        assert!(!args_request_help(&[
            "note".to_string(),
            "--text".to_string(),
            "-h".to_string(),
        ]));
        // A `--help`-shaped free-text value is equally inert.
        assert!(!args_request_help(&[
            "note".to_string(),
            "--text".to_string(),
            "--help".to_string(),
        ]));
        // A positional -h / --help still triggers help.
        assert!(args_request_help(&["note".to_string(), "-h".to_string()]));
        assert!(args_request_help(&[
            "note".to_string(),
            "--help".to_string()
        ]));
        // Structured flag consuming a `-h` value stays tokenizer-consistent:
        // the value binds to the flag (parse_flags would too), so no help.
        assert!(!args_request_help(&[
            "--task".to_string(),
            "-h".to_string()
        ]));
    }

    #[test]
    fn next_value_rejects_flag_shaped_value() {
        // `--task --json` must error (missing value for --task) rather than
        // binding task="--json".
        let args = vec!["--task".to_string(), "--json".to_string()];
        match parse_flags(&args) {
            Err(CliError::Usage(msg)) => assert!(
                msg.contains("--task"),
                "expected --task missing-value error, got: {msg}"
            ),
            Err(other) => panic!("expected CliError::Usage, got {other:?}"),
            Ok(f) => panic!("expected --task to error, but parsed task={:?}", f.task),
        }
    }

    #[test]
    fn next_value_accepts_flag_shaped_value_for_free_text_flags() {
        // Free-text flags must accept a `--`-prefixed value verbatim.
        let args = vec!["--text".to_string(), "--foo bar".to_string()];
        match parse_flags(&args) {
            Ok(f) => assert_eq!(f.text.as_deref(), Some("--foo bar")),
            Err(e) => panic!("expected --text to accept '--foo bar', got {e:?}"),
        }
    }
}
