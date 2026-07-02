//! CLI subcommands for the orchestrator layer.
//!
//! These run before the TUI starts up — when argv contains a known verb,
//! [`dispatch`] handles it and returns an exit code. The TUI in `main.rs`
//! never sees them.
//!
//! Argument parsing is hand-rolled to avoid a clap dep. The orchestrator-facing
//! verbs include `spawn-worker`, `merge-worktree`, `task ...`, `orchestrate ...`,
//! `pr ...`, `worker ...`, and `project ...`.
//!
//! Most verbs derive `project-id` from the current working directory by default;
//! `--project-id ID` overrides for the rare case of operating cross-project.
//! They emit a single JSON line on stdout describing the result so the
//! orchestrator (a Claude or Pi session running under Bash) can parse the
//! outcome programmatically.
//!
//! Worktree mechanics live here too: `git -C <root> worktree add -b <branch>
//! <path> main`. cc-hub does **only** the mechanical git ops; deciding when
//! to spawn one and when to merge is the orchestrator's job.

mod help;
mod merge_worktree;
mod orchestrate;
mod pr;
mod project;
mod spawn_worker;
mod task;
#[cfg(test)]
mod test_util;
mod worker;

use cc_hub_lib::ops::{self, OpError};
use cc_hub_lib::orchestrator;

pub fn dispatch(args: &[String]) -> Option<i32> {
    let (verb, rest) = args.split_first()?;
    if matches!(verb.as_str(), "help" | "--help" | "-h") {
        return Some(handle(help::print_cli_help(rest)));
    }
    if args_request_help(rest) {
        return Some(handle(help::print_cli_help(args)));
    }
    match verb.as_str() {
        "spawn-worker" => Some(handle(spawn_worker::spawn_worker(rest))),
        "merge-worktree" => Some(handle(merge_worktree::merge_worktree(rest))),
        "task" => Some(handle(task::task_subcommand(rest))),
        "orchestrate" => Some(handle(orchestrate::orchestrate_subcommand(rest))),
        "pr" => Some(handle(pr::pr_subcommand(rest))),
        "worker" => Some(handle(worker::worker_subcommand(rest))),
        "project" => Some(handle(project::project_subcommand(rest))),
        _ => None,
    }
}

/// Terminate a verb by emitting the orchestrator-facing contract and
/// returning the process exit code.
///
/// The module contract is "one JSON line on stdout". On success the verb
/// already printed its own JSON; on error we print a structured error line
/// here — `{"ok":false,"error":"<msg>","kind":"<usage|notfound|conflict|
/// other>"}` (plus `recipe` when the variant carries a hint) — so an
/// orchestrator piping stdout to `jq` always sees a parseable result, even
/// on the failures that matter most. A short human line still goes to
/// stderr for interactive use.
fn handle(result: Result<(), CliError>) -> i32 {
    match result {
        Ok(()) => 0,
        // The verb already printed a richer `{"ok":false,...}` line (e.g. the
        // merge-lock-held / merge-conflict payloads carrying domain fields the
        // generic shape can't express). Don't double-print JSON — just emit a
        // human line and the exit code.
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
    /// Requested entity does not exist (no task / no PR). Exit 1,
    /// kind "notfound".
    NotFound(String),
    /// State guard tripped: merge lock held, conflicting transition on a
    /// terminal PR, etc. Exit 1, kind "conflict". Carries an optional
    /// recipe the orchestrator can act on.
    Conflict { msg: String, recipe: Option<String> },
    /// Everything else (I/O, git failures, serialization). Exit 1,
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
    worktree: Option<String>,
    readonly: bool,
    prompt: Option<String>,
    agent: Option<String>,
    project_id: Option<String>,
    status: Option<String>,
    note: Option<String>,
    summary: Option<String>,
    path: Option<String>,
    kind: Option<String>,
    caption: Option<String>,
    items: Option<String>,
    index: Option<usize>,
    lead: bool,
    wait_secs: Option<u64>,
    dry_run: bool,
    backlog: bool,
    /// `task create --name NAME` — project display name when registering a
    /// fresh project from the cwd. Ignored when `--project-id` is given.
    name: Option<String>,
    /// PR-flow flags. `--title` / `--description` for `pr create`,
    /// `--comment` + `--author` for `pr request-changes` / `pr comment`.
    title: Option<String>,
    description: Option<String>,
    comment: Option<String>,
    author: Option<String>,
    /// `pr show --comments-since UNIX_TS` — only emit comments whose `at`
    /// timestamp is `>= UNIX_TS`. Boundary is inclusive so passing the
    /// previous `pr.updated_at` returns any comments stamped at exactly
    /// that second.
    comments_since: Option<i64>,
    /// `worker wait` flags. Repeatable `--tmux NAME`, `--all`,
    /// `--timeout-secs N`. `worktree_targets` is populated alongside
    /// `worktree` (Option) by the `--worktree` parser arm: single-use
    /// callers like `spawn-worker` / `pr create` read `worktree`,
    /// `worker wait` reads `worktree_targets` so it can take repeats.
    tmux_targets: Vec<String>,
    worktree_targets: Vec<String>,
    all: bool,
    timeout_secs: Option<u64>,
    progress: bool,
    progress_interval_secs: Option<u64>,
    json: bool,
    /// `pr lock-phase` — one of merging|simplify|bump|finalize-pending.
    phase: Option<String>,
    /// `task delete --force` — required for Running/Review/Merging tasks.
    /// Deleting a Merging task releases the project merge lock (recovery path
    /// for a wedged merge with a dead orchestrator).
    force: bool,
    /// `pr finalize` flags: override the build command, skip the build
    /// gate entirely, or keep the orchestrator tmux session alive.
    build_cmd: Option<String>,
    skip_build: bool,
    keep_tmux: bool,
    wait: bool,
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
            "--worktree" => {
                let v = next_value(args, &mut i, "--worktree")?;
                // Reject names that aren't `^[A-Za-z0-9][A-Za-z0-9._-]*$` at the
                // chokepoint: the value is embedded into `git worktree add -b`
                // and into the auto-reviewer's shell instructions an unattended
                // LLM runs (a leading dash hits git as a flag; exotic chars open
                // prompt/shell injection).
                if !orchestrator::is_valid_worktree_name(&v) {
                    return Err(CliError::Usage(format!(
                        "--worktree {}: must match ^[A-Za-z0-9][A-Za-z0-9._-]*$ (letters, digits, '.', '_', '-'; no leading dash, slashes, or spaces)",
                        v
                    )));
                }
                f.worktree = Some(v.clone());
                f.worktree_targets.push(v);
            }
            "--readonly" => {
                f.readonly = true;
                i += 1;
            }
            "--prompt" => {
                f.prompt = Some(next_value(args, &mut i, "--prompt")?);
            }
            "--agent" => {
                f.agent = Some(next_value(args, &mut i, "--agent")?);
            }
            "--project-id" => {
                f.project_id = Some(next_value(args, &mut i, "--project-id")?);
            }
            "--status" => {
                f.status = Some(next_value(args, &mut i, "--status")?);
            }
            "--note" => {
                f.note = Some(next_value(args, &mut i, "--note")?);
            }
            "--summary" => {
                f.summary = Some(next_value(args, &mut i, "--summary")?);
            }
            "--path" => {
                f.path = Some(next_value(args, &mut i, "--path")?);
            }
            "--kind" => {
                f.kind = Some(next_value(args, &mut i, "--kind")?);
            }
            "--caption" => {
                f.caption = Some(next_value(args, &mut i, "--caption")?);
            }
            "--items" => {
                f.items = Some(next_value(args, &mut i, "--items")?);
            }
            "--index" => {
                let v = next_value(args, &mut i, "--index")?;
                f.index = Some(
                    v.parse()
                        .map_err(|e| CliError::Usage(format!("--index: {}", e)))?,
                );
            }
            "--lead" => {
                f.lead = true;
                i += 1;
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
            "--backlog" => {
                f.backlog = true;
                i += 1;
            }
            "--name" => {
                f.name = Some(next_value(args, &mut i, "--name")?);
            }
            "--title" => {
                f.title = Some(next_value(args, &mut i, "--title")?);
            }
            "--description" => {
                f.description = Some(next_value(args, &mut i, "--description")?);
            }
            "--comment" => {
                f.comment = Some(next_value(args, &mut i, "--comment")?);
            }
            "--author" => {
                f.author = Some(next_value(args, &mut i, "--author")?);
            }
            "--comments-since" => {
                let v = next_value(args, &mut i, "--comments-since")?;
                f.comments_since = Some(
                    v.parse()
                        .map_err(|e| CliError::Usage(format!("--comments-since: {}", e)))?,
                );
            }
            "--tmux" => {
                f.tmux_targets.push(next_value(args, &mut i, "--tmux")?);
            }
            "--all" => {
                f.all = true;
                i += 1;
            }
            "--wait" => {
                f.wait = true;
                i += 1;
            }
            "--timeout-secs" => {
                let v = next_value(args, &mut i, "--timeout-secs")?;
                f.timeout_secs = Some(
                    v.parse()
                        .map_err(|e| CliError::Usage(format!("--timeout-secs: {}", e)))?,
                );
            }
            "--progress" => {
                f.progress = true;
                i += 1;
            }
            "--progress-interval-secs" => {
                let v = next_value(args, &mut i, "--progress-interval-secs")?;
                f.progress_interval_secs =
                    Some(v.parse().map_err(|e| {
                        CliError::Usage(format!("--progress-interval-secs: {}", e))
                    })?);
            }
            "--json" => {
                f.json = true;
                i += 1;
            }
            "--phase" => {
                f.phase = Some(next_value(args, &mut i, "--phase")?);
            }
            "--force" => {
                f.force = true;
                i += 1;
            }
            "--build-cmd" => {
                f.build_cmd = Some(next_value(args, &mut i, "--build-cmd")?);
            }
            "--skip-build" => {
                f.skip_build = true;
                i += 1;
            }
            "--keep-tmux" => {
                f.keep_tmux = true;
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
/// (e.g. `--build-cmd "--release"`, `--prompt "--foo bar"`,
/// `--summary "--wip notes"`). For these we accept the next token verbatim.
const FREE_TEXT_FLAGS: &[&str] = &[
    "--prompt",
    "--note",
    "--summary",
    "--title",
    "--description",
    "--comment",
    "--build-cmd",
    "--name",
    "--caption",
    "--items",
    "--author",
];

fn next_value(args: &[String], i: &mut usize, name: &str) -> Result<String, CliError> {
    *i += 1;
    // For STRUCTURED flags (ids, enums, numbers, paths), a value that looks like
    // another flag means the caller omitted the value (e.g. `--task --status
    // running` would otherwise bind task="--status") — reject it. Free-text
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
const BOOL_FLAGS: &[&str] = &[
    "--readonly",
    "--lead",
    "--dry-run",
    "--backlog",
    "--all",
    "--wait",
    "--progress",
    "--json",
    "--force",
    "--skip-build",
    "--keep-tmux",
];

/// Flag-aware scan for a help request among a verb's args.
///
/// A naive `args.iter().any(|a| a == "-h" || a == "--help")` misfires when a
/// flag VALUE equals `-h`/`--help` — e.g. `pr comment --comment "-h"` would
/// silently reroute to help and exit 0, which the orchestrator reads as a
/// success no-op. So mirror [`next_value`]'s tokenizer: a value-consuming flag
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

fn resolve_project_id(f: &Flags) -> Result<String, CliError> {
    if let Some(id) = f.project_id.clone() {
        return Ok(id);
    }
    let cwd = std::env::current_dir().map_err(|e| CliError::Other(format!("cwd: {}", e)))?;
    let cwd_id = orchestrator::project_id_for_path(&cwd);

    // Fast path: if the cwd id has a state file for this task (or no --task
    // was passed), keep current behavior. Otherwise fall through and scan.
    if cwd_id_has_task_state(&cwd_id, f.task.as_deref()) {
        return Ok(cwd_id);
    }
    let Some(task_id) = f.task.as_deref() else {
        return Ok(cwd_id);
    };

    let matches = scan_projects_for_task(task_id);
    match matches.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err(CliError::Other(format!(
            "task {} not found under any registered project (cwd id {} has no state file); pass --project-id to disambiguate",
            task_id, cwd_id
        ))),
        many => Err(CliError::Other(format!(
            "task {} matches multiple registered projects: {}; pass --project-id to disambiguate",
            task_id,
            many.join(", ")
        ))),
    }
}

/// True when `--task` is absent (cwd id is fine on its own) or when the
/// per-task state.json exists under the cwd-derived project id.
fn cwd_id_has_task_state(cwd_id: &str, task_id: Option<&str>) -> bool {
    let Some(task_id) = task_id else {
        return true;
    };
    orchestrator::task_state_file(cwd_id, task_id)
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Enumerate `~/.cc-hub/projects/*/tasks/<task_id>/state.json` and return the
/// project ids whose directory contains a state file for the given task.
fn scan_projects_for_task(task_id: &str) -> Vec<String> {
    let Some(root) = orchestrator::projects_state_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Some(project_id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(state_file) = orchestrator::task_state_file(&project_id, task_id) else {
            continue;
        };
        if state_file.exists() {
            out.push(project_id);
        }
    }
    out.sort();
    out
}

fn print_json(value: &serde_json::Value) {
    // One line per call so orchestrators can split on \n. Pretty-print would
    // make Bash piping awkward.
    match serde_json::to_string(value) {
        Ok(s) => println!("{}", s),
        Err(e) => eprintln!("(failed to serialise output: {})", e),
    }
}

/// Map a [`PromptStatus`] to its JSON string, emitting the human warning line
/// to stderr for the `Deferred` case (presentation stays in cli.rs).
fn report_prompt_status(status: &ops::worker::PromptStatus) -> &'static str {
    if let ops::worker::PromptStatus::Deferred(warning) = status {
        eprintln!("warning: {}", warning);
    }
    status.as_str()
}

#[cfg(test)]
mod tests {
    use super::test_util::with_tempdir_home;
    use super::*;
    use cc_hub_lib::orchestrator::TaskState;
    use std::path::PathBuf;

    #[test]
    fn dispatch_handles_help() {
        assert_eq!(dispatch(&["--help".to_string()]), Some(0));
        assert_eq!(
            dispatch(&["task".to_string(), "--help".to_string()]),
            Some(0)
        );
    }

    #[test]
    fn help_scan_ignores_free_text_flag_value() {
        // Regression: `pr comment --comment "-h"` — the `-h` is the comment
        // VALUE, not a help request. Rerouting to help here is a silent exit-0
        // no-op the orchestrator misreads as success.
        assert!(!args_request_help(&[
            "comment".to_string(),
            "--comment".to_string(),
            "-h".to_string(),
        ]));
        // A `--help`-shaped free-text value is equally inert.
        assert!(!args_request_help(&[
            "comment".to_string(),
            "--comment".to_string(),
            "--help".to_string(),
        ]));
        // A positional -h / --help still triggers help.
        assert!(args_request_help(&[
            "comment".to_string(),
            "-h".to_string()
        ]));
        assert!(args_request_help(&[
            "comment".to_string(),
            "--help".to_string()
        ]));
        // Structured flag consuming a `-h` value stays tokenizer-consistent:
        // the value binds to the flag (parse_flags would too), so no help.
        assert!(!args_request_help(&[
            "--status".to_string(),
            "-h".to_string()
        ]));
    }

    #[test]
    fn next_value_rejects_flag_shaped_value() {
        // `--task --status running` must error (missing value for --task)
        // rather than binding task="--status".
        let args = vec![
            "--task".to_string(),
            "--status".to_string(),
            "running".to_string(),
        ];
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
        // Free-text flags must accept a `--`-prefixed value verbatim —
        // e.g. `--build-cmd "--release"` is a legitimate build command.
        let args = vec!["--build-cmd".to_string(), "--release".to_string()];
        match parse_flags(&args) {
            Ok(f) => assert_eq!(f.build_cmd.as_deref(), Some("--release")),
            Err(e) => panic!("expected --build-cmd to accept '--release', got {e:?}"),
        }
        // And a free-text prompt that begins with dashes survives intact.
        let args = vec!["--prompt".to_string(), "--foo bar".to_string()];
        match parse_flags(&args) {
            Ok(f) => assert_eq!(f.prompt.as_deref(), Some("--foo bar")),
            Err(e) => panic!("expected --prompt to accept '--foo bar', got {e:?}"),
        }
    }

    fn write_state_for(project_id: &str, task_id: &str) {
        let mut state = TaskState::new(
            project_id.to_string(),
            PathBuf::from("/tmp/proj"),
            "do thing".into(),
        );
        state.task_id = task_id.to_string();
        orchestrator::write_task_state(&state).expect("write state");
    }

    fn flags_for(task: Option<&str>, project_id: Option<&str>) -> Flags {
        Flags {
            task: task.map(str::to_owned),
            project_id: project_id.map(str::to_owned),
            ..Flags::default()
        }
    }

    #[test]
    fn resolve_project_id_returns_explicit_flag_verbatim() {
        with_tempdir_home(|| {
            // No state files anywhere — explicit --project-id must still be
            // returned without triggering the fallback scan.
            let f = flags_for(Some("t-anything"), Some("p-explicit"));
            let got = resolve_project_id(&f).expect("resolve ok");
            assert_eq!(got, "p-explicit");
        });
    }

    #[test]
    fn resolve_project_id_scans_when_cwd_id_misses() {
        with_tempdir_home(|| {
            // Real project id where the task lives. cwd at test time is the
            // workspace dir, which canonicalizes to a different project id —
            // so the cwd fast-path will miss and the scan must find p-real.
            let task_id = "t-scan";
            write_state_for("p-real", task_id);

            let cwd = std::env::current_dir().expect("cwd");
            let cwd_id = orchestrator::project_id_for_path(&cwd);
            assert_ne!(cwd_id, "p-real", "test precondition: cwd id must differ");

            let f = flags_for(Some(task_id), None);
            let got = resolve_project_id(&f).expect("resolve ok");
            assert_eq!(got, "p-real");
        });
    }

    #[test]
    fn resolve_project_id_errors_with_candidates_on_multiple_matches() {
        with_tempdir_home(|| {
            let task_id = "t-dup";
            write_state_for("p-alpha", task_id);
            write_state_for("p-beta", task_id);

            let f = flags_for(Some(task_id), None);
            let err = resolve_project_id(&f).expect_err("must be ambiguous");
            let CliError::Other(msg) = err else {
                panic!("expected CliError::Other, got {:?}", err);
            };
            assert!(msg.contains("p-alpha"), "msg should list p-alpha: {}", msg);
            assert!(msg.contains("p-beta"), "msg should list p-beta: {}", msg);
            assert!(
                msg.contains("--project-id"),
                "msg should mention --project-id: {}",
                msg
            );
        });
    }

    #[test]
    fn resolve_project_id_errors_when_task_not_registered() {
        with_tempdir_home(|| {
            let f = flags_for(Some("t-nope"), None);
            let err = resolve_project_id(&f).expect_err("must error");
            let CliError::Other(msg) = err else {
                panic!("expected CliError::Other, got {:?}", err);
            };
            assert!(
                msg.contains("t-nope"),
                "msg should mention task id: {}",
                msg
            );
            assert!(
                msg.contains("--project-id"),
                "msg should suggest --project-id: {}",
                msg
            );
        });
    }

    #[test]
    fn resolve_project_id_without_task_falls_back_to_cwd_id() {
        with_tempdir_home(|| {
            // No --task means the scan is skipped; cwd id is returned as-is
            // even if no state files exist anywhere. This preserves verbs
            // like `project list` that don't need a task.
            let f = flags_for(None, None);
            let got = resolve_project_id(&f).expect("resolve ok");
            let cwd = std::env::current_dir().expect("cwd");
            assert_eq!(got, orchestrator::project_id_for_path(&cwd));
        });
    }

    #[test]
    fn parse_flags_rejects_unsafe_worktree_names() {
        // Leading dash (would hit `git worktree add -b` as a flag), path
        // traversal, and whitespace must all be rejected at the chokepoint.
        for bad in ["-foo", "../x", "a b", ".hidden", "x/y"] {
            let args = vec!["--worktree".to_string(), bad.to_string()];
            match parse_flags(&args) {
                Err(CliError::Usage(msg)) => assert!(
                    msg.contains("--worktree"),
                    "expected a --worktree usage error for {:?}, got: {msg}",
                    bad
                ),
                Err(other) => panic!("expected Usage error for {:?}, got {:?}", bad, other),
                Ok(_) => panic!("expected {:?} to be rejected", bad),
            }
        }
    }

    #[test]
    fn parse_flags_accepts_safe_worktree_names() {
        for good in ["fix", "fix-123", "v1.2_x", "a"] {
            let args = vec!["--worktree".to_string(), good.to_string()];
            let f = parse_flags(&args).unwrap_or_else(|e| {
                panic!("expected {:?} to parse, got {:?}", good, e);
            });
            assert_eq!(f.worktree.as_deref(), Some(good));
        }
    }
}
