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

use cc_hub_lib::models;
use cc_hub_lib::ops::{self, OpError};
use cc_hub_lib::orchestrator::{self, MergeOutcome, TaskState, TaskStatus, TodoItem};
use std::path::PathBuf;
use std::time::Duration;

const TASK_VERBS_HELP: &str =
    "`report`, `create`, `start`, `list`, `show`, `delete`, `gc`, `auto-review`, `artifact`, or `todos`";

pub fn dispatch(args: &[String]) -> Option<i32> {
    let (verb, rest) = args.split_first()?;
    if matches!(verb.as_str(), "help" | "--help" | "-h") {
        return Some(handle(print_cli_help(rest)));
    }
    if rest.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        return Some(handle(print_cli_help(args)));
    }
    match verb.as_str() {
        "spawn-worker" => Some(handle(spawn_worker(rest))),
        "merge-worktree" => Some(handle(merge_worktree(rest))),
        "task" => Some(handle(task_subcommand(rest))),
        "orchestrate" => Some(handle(orchestrate_subcommand(rest))),
        "pr" => Some(handle(pr_subcommand(rest))),
        "worker" => Some(handle(worker_subcommand(rest))),
        "project" => Some(handle(project_subcommand(rest))),
        _ => None,
    }
}

fn print_cli_help(topic: &[String]) -> Result<(), CliError> {
    match topic.first().map(String::as_str) {
        None => print!("{}", GENERAL_HELP),
        Some("spawn-worker") => print!("{}", SPAWN_WORKER_HELP),
        Some("merge-worktree") => print!("{}", MERGE_WORKTREE_HELP),
        Some("task") => print!("{}", TASK_HELP),
        Some("orchestrate") => print!("{}", ORCHESTRATE_HELP),
        Some("pr") => print!("{}", PR_HELP),
        Some("worker") => print!("{}", WORKER_HELP),
        Some("project") => print!("{}", PROJECT_HELP),
        Some(other) => {
            return Err(CliError::Usage(format!(
                "unknown help topic: {} (try `cc-hub help`)",
                other
            )));
        }
    }
    Ok(())
}

const GENERAL_HELP: &str = r#"cc-hub

Usage:
  cc-hub                         Start the TUI
  cc-hub --no-tui                Print discovered sessions
  cc-hub help [topic]            Show CLI help

Orchestrator-facing topics:
  spawn-worker      Spawn a readonly or worktree worker for a task
  merge-worktree    Legacy direct worktree merge helper
  task              Create/report/start tasks, artifacts, and todos
  orchestrate       Spawn an orchestrator for an existing task
  pr                Local PR review/merge flow
  worker            Wait for worker sessions to finish
  project           List registered projects

Examples:
  cc-hub task create --backlog --prompt "Fix the flaky test"
  cc-hub task start --task t-123 --agent claude
  cc-hub spawn-worker --task t-123 --worktree fix --prompt "Implement the fix"
  cc-hub pr show --task t-123
"#;

const SPAWN_WORKER_HELP: &str = r#"cc-hub spawn-worker

Usage:
  cc-hub spawn-worker --task ID (--worktree NAME | --readonly) [options]

Options:
  --project-id ID      Override inferred project id
  --agent AGENT        Worker backend (defaults to task orchestrator agent)
  --prompt TEXT        Initial prompt to send to the worker
  --wait-secs N        Prompt-dispatch readiness timeout (default: 120)

Emits one JSON line with tmux/cwd/worktree/prompt_status.
"#;

const MERGE_WORKTREE_HELP: &str = r#"cc-hub merge-worktree

Usage:
  cc-hub merge-worktree --task ID --worktree NAME [--project-id ID]

Legacy helper that merges a worker branch into the project's main branch and
records a MergeRecord. New PR-flow tasks generally use `cc-hub pr merge`.
"#;

const TASK_HELP: &str = r#"cc-hub task

Usage:
  cc-hub task create --prompt TEXT [--backlog] [--name NAME] [--project-id ID]
  cc-hub task start --task ID [--agent AGENT] [--wait-secs N] [--project-id ID]
  cc-hub task report --task ID [--status running|review|merging|done|backlog] [--note TEXT] [--summary TEXT]
  cc-hub task show --task ID [--project-id ID] [--json]
  cc-hub task delete --task ID [--project-id ID] [--force]
  cc-hub task gc [--project-id ID] [--dry-run]
  cc-hub task auto-review --task ID [--project-id ID]
  cc-hub task list [--status backlog|running|review|merging|done] [--project-id ID] [--json]
  cc-hub task artifact add --task ID --path PATH_OR_URL [--kind KIND] [--caption TEXT] [--lead]
  cc-hub task artifact list --task ID [--project-id ID]
  cc-hub task todos set --task ID --items JSON_ARRAY
  cc-hub task todos check|uncheck --task ID --index N
  cc-hub task todos clear --task ID

All mutating verbs emit one JSON line. `report --status done` routes a running
task into Review first so a human/reviewer can approve it.
"#;

const ORCHESTRATE_HELP: &str = r#"cc-hub orchestrate

Usage:
  cc-hub orchestrate start --task ID [--agent AGENT] [--wait-secs N] [--dry-run]

Spawns the configured orchestrator backend in the task's project root, persists
its tmux session name, and sends the generated orchestrator prompt.
"#;

const PR_HELP: &str = r#"cc-hub pr

Usage:
  cc-hub pr create --task ID --worktree NAME --title TEXT [--description TEXT]
  cc-hub pr show --task ID
  cc-hub pr approve --task ID
  cc-hub pr request-changes --task ID --comment TEXT [--author NAME]
  cc-hub pr reopen --task ID [--comment TEXT] [--author NAME]
  cc-hub pr comment --task ID --comment TEXT [--author NAME]
  cc-hub pr close --task ID [--project-id ID] [--comment TEXT] [--author NAME]
  cc-hub pr merge --task ID [--wait [--timeout-secs N]]
  cc-hub pr continue --task ID [--project-id ID]
  cc-hub pr lock-phase --task ID --phase merging|simplify|bump|finalize-pending
  cc-hub pr finalize --task ID [--build-cmd CMD] [--skip-build] [--keep-tmux]

Local PR records live beside task state. Merges are serialized with the
project merge lock; `finalize` releases the lock and marks the task Done.

`pr merge` acquires the project merge lock. If another task holds it, the
default is to fail fast with `{ok:false, locked:true, ...}`; pass `--wait` to
block until the lock frees (bounded by `--timeout-secs N`, default 1800).

`pr continue` re-pings the task's orchestrator with the merge-flow prompt
(the same one the TUI sends on approve). Idempotent — safe to re-run. If the
orchestrator session is dead it reports `{ok:false, orchestrator_alive:false}`
with a recipe to resurrect or `task delete --force` the wedged task.
"#;

const WORKER_HELP: &str = r#"cc-hub worker

Usage:
  cc-hub worker wait --task ID (--tmux NAME ... | --worktree NAME ... | --all)
                     [--timeout-secs N] [--progress [--progress-interval-secs N]]

Polls cc-hub's session scanner until selected workers reach WaitingForInput,
Question, or Inactive. Emits one JSON line with per-worker completion state.

With --progress, emits one JSON line every N seconds (default 5) describing
which targets are still pending vs. done. The final summary line is unchanged.
"#;

const PROJECT_HELP: &str = r#"cc-hub project

Usage:
  cc-hub project list [--json]

Lists registered projects. Plain output is tab-separated:
  <id>\t<name>\t<root>
With --json, includes per-status task counts.
"#;

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

// ─── spawn-worker ─────────────────────────────────────────────────────────

fn spawn_worker(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let outcome = ops::worker::spawn_worker(
        &project_id,
        &task_id,
        ops::worker::SpawnWorkerOpts {
            worktree: f.worktree.clone(),
            readonly: f.readonly,
            prompt: f.prompt.clone(),
            agent: f.agent.clone(),
            wait_secs: f.wait_secs,
        },
    )?;

    let prompt_status = report_prompt_status(&outcome.prompt_status);

    print_json(&serde_json::json!({
        "ok": true,
        "agent_id": outcome.agent_id,
        "agent_kind": outcome.agent_kind,
        "tmux": outcome.tmux,
        "cwd": outcome.cwd,
        "worktree": outcome.worktree,
        "readonly": outcome.readonly,
        "prompt_status": prompt_status,
        "task_id": task_id,
        "project_id": project_id,
    }));
    Ok(())
}

/// Map a [`PromptStatus`] to its JSON string, emitting the human warning line
/// to stderr for the `Deferred` case (presentation stays in cli.rs).
fn report_prompt_status(status: &ops::worker::PromptStatus) -> &'static str {
    if let ops::worker::PromptStatus::Deferred(warning) = status {
        eprintln!("warning: {}", warning);
    }
    status.as_str()
}

// ─── merge-worktree ───────────────────────────────────────────────────────

fn merge_worktree(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let worktree_name = f
        .worktree
        .clone()
        .ok_or_else(|| CliError::Usage("--worktree NAME is required".into()))?;

    let result = ops::worker::merge_worktree(&project_id, &task_id, &worktree_name)?;
    let ops::worker::MergeWorktreeOutcome {
        outcome,
        worktree,
        branch,
        main,
        stdout,
        stderr,
    } = result;

    let mut payload = serde_json::json!({
        "ok": matches!(outcome, MergeOutcome::Ok),
        "worktree": worktree,
        "branch": branch,
        "main": main,
        "stdout": stdout,
        "stderr": stderr,
    });
    if let MergeOutcome::BlockedByDirtyTree { overlap } = &outcome {
        payload["blocked_by_dirty_tree"] = serde_json::json!(true);
        payload["overlap"] = serde_json::json!(overlap);
        payload["recipe"] = serde_json::json!(
            "Commit, stash, or revert the listed paths on the target branch, then re-run `cc-hub merge-worktree`."
        );
    }
    print_json(&payload);

    // The outcome payload was already printed above; use `Reported` so `handle()`
    // doesn't emit a SECOND JSON line on stdout (which would break the
    // one-line-per-call contract for an orchestrator piping to `jq`).
    match outcome {
        MergeOutcome::Ok => Ok(()),
        MergeOutcome::Conflict { .. } => Err(CliError::Reported(
            "merge produced conflicts; resolve in the worktree or main".into(),
        )),
        MergeOutcome::BlockedByDirtyTree { overlap } => Err(CliError::Reported(format!(
            "merge blocked: working tree on `{}` has uncommitted edits in {} file(s) the branch also modified ({}); commit/stash/revert and retry",
            main,
            overlap.len(),
            overlap.join(", ")
        ))),
    }
}

// ─── orchestrate ─────────────────────────────────────────────────────────

fn orchestrate_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args
        .split_first()
        .ok_or_else(|| CliError::Usage("orchestrate <verb>: missing verb (try `start`)".into()))?;
    match verb.as_str() {
        "start" => orchestrate_start(rest),
        other => Err(CliError::Usage(format!(
            "unknown orchestrate verb: {} (try `start`)",
            other
        ))),
    }
}

/// `cc-hub orchestrate start --task ID [--project-id ID] [--wait-secs N]`
///
/// Spawns the selected orchestrator backend in the project root, waits up to `--wait-secs` (default
/// 60) for the new session to reach Idle, then dispatches the orchestrator
/// prompt as the first user message. Records the resulting tmux name in
/// state.json.
fn orchestrate_start(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let outcome = ops::task::orchestrate_start(
        &project_id,
        &task_id,
        f.agent.clone(),
        f.wait_secs,
        f.dry_run,
    )?;

    let spawn = match outcome {
        ops::task::OrchestrateStart::DryRun(prompt) => {
            // Useful for verifying prompt content without paying for a session.
            println!("{}", prompt);
            return Ok(());
        }
        ops::task::OrchestrateStart::Spawned(spawn) => spawn,
    };

    let prompt_status = report_prompt_status(&spawn.prompt_status);

    print_json(&serde_json::json!({
        "ok": true,
        "agent_id": spawn.state.orchestrator_agent_id,
        "agent_kind": spawn.state.orchestrator_agent_kind,
        "tmux": spawn.tmux,
        "cwd": spawn.state.project_root.to_string_lossy(),
        "prompt_status": prompt_status,
        "task_id": task_id,
        "project_id": project_id,
    }));
    Ok(())
}

// ─── task ────────────────────────────────────────────────────────────────

fn task_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args.split_first().ok_or_else(|| {
        CliError::Usage(format!(
            "task <verb>: missing verb (try {})",
            TASK_VERBS_HELP
        ))
    })?;
    match verb.as_str() {
        "report" => task_report(rest),
        "create" => task_create(rest),
        "start" => task_start(rest),
        "show" => task_show(rest),
        "list" => task_list(rest),
        "delete" => task_delete(rest),
        "gc" => task_gc(rest),
        "auto-review" => task_auto_review(rest),
        "artifact" => task_artifact_subcommand(rest),
        "todos" => task_todos_subcommand(rest),
        other => Err(CliError::Usage(format!(
            "unknown task verb: {} (try {})",
            other, TASK_VERBS_HELP
        ))),
    }
}

/// `cc-hub task delete --task ID [--project-id ID] [--force]`
///
/// End-to-end teardown: kills the orchestrator tmux (best-effort), removes
/// every worktree the task owns, then deletes the on-disk task state dir.
/// Requires `--force` for `Running` / `Review` / `Merging` so the user has to
/// acknowledge they're killing live work. `Merging` is recoverable under
/// `--force`: `delete_task` releases the merge lock so a wedged task (e.g. a
/// dead orchestrator that never ran `pr finalize`) doesn't leave the project
/// merge lock held forever.
fn task_delete(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let deleted = ops::task::task_delete(&project_id, &task_id, f.force)?;

    let worktree_errors: Vec<serde_json::Value> = deleted
        .worktree_errors
        .iter()
        .map(|(path, err)| {
            serde_json::json!({
                "path": path,
                "error": err,
            })
        })
        .collect();

    print_json(&serde_json::json!({
        "ok": true,
        "task_id": deleted.task_id,
        "project_id": deleted.project_id,
        "orchestrator_killed": deleted.orchestrator_killed,
        "lock_released": deleted.lock_released,
        "state_removed": deleted.state_removed,
        "worktrees_removed": deleted.worktrees_removed,
        "worktree_errors": worktree_errors,
    }));
    Ok(())
}

/// `cc-hub task gc [--project-id ID] [--dry-run]`
///
/// Sweep orphaned worktrees under `<root>/.cc-hub-wt/`. Worktrees + their
/// `cc-hub/*` branches are otherwise only torn down on the Done path, so
/// Review / abandoned / wedged tasks leak them. This removes every worktree
/// no live (present, non-Done) task owns, deletes their dangling branches, and
/// runs `git worktree prune`. `--dry-run` prints the plan as JSON without
/// acting.
fn task_gc(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    // No `--task` for gc; resolve_project_id falls back to the cwd-derived id.
    let project_id = resolve_project_id(&f)?;
    let project_root = resolve_project_root(&project_id)?;

    let outcome = ops::task::task_gc(&project_id, &project_root, f.dry_run)?;

    let orphans: Vec<serde_json::Value> = outcome
        .orphans
        .iter()
        .map(|w| {
            serde_json::json!({
                "dir_name": w.dir_name,
                "path": w.path.to_string_lossy(),
                "branch": w.branch,
            })
        })
        .collect();
    let live: Vec<serde_json::Value> = outcome
        .live
        .iter()
        .map(|w| {
            serde_json::json!({
                "dir_name": w.dir_name,
                "path": w.path.to_string_lossy(),
                "branch": w.branch,
            })
        })
        .collect();
    let errors: Vec<serde_json::Value> = outcome
        .errors
        .iter()
        .map(|(path, err)| serde_json::json!({ "path": path, "error": err }))
        .collect();

    print_json(&serde_json::json!({
        "ok": true,
        "project_id": project_id,
        "dry_run": f.dry_run,
        "orphans": orphans,
        "live": live,
        "worktrees_removed": outcome.worktrees_removed,
        "branches_removed": outcome.branches_removed,
        "errors": errors,
        "pruned": outcome.pruned,
    }));
    Ok(())
}

/// Resolve a registered project's root by id, falling back to the cwd when the
/// id matches the cwd-derived id (so `task gc` works from a project dir that
/// hasn't been explicitly registered yet, mirroring `resolve_project_id`'s
/// cwd fallback).
fn resolve_project_root(project_id: &str) -> Result<PathBuf, CliError> {
    if let Some(p) = orchestrator::load_projects()
        .projects
        .into_iter()
        .find(|p| p.id == project_id)
    {
        return Ok(p.root);
    }
    let cwd = std::env::current_dir().map_err(|e| CliError::Other(format!("cwd: {}", e)))?;
    if orchestrator::project_id_for_path(&cwd) == project_id {
        return Ok(cwd);
    }
    Err(CliError::NotFound(format!(
        "project {} is not registered and does not match the current directory; pass --project-id for a registered project",
        project_id
    )))
}

/// `cc-hub task start --task ID [--project-id ID] [--agent ID] [--wait-secs N]`
///
/// Flip a Backlog task to Running and spawn its orchestrator. Mirrors
/// `orchestrate start` but goes through `start_backlog_task` so it errors
/// cleanly if the task isn't in Backlog.
fn task_start(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let spawn = ops::task::task_start(&project_id, &task_id, f.agent.clone(), f.wait_secs)?;

    let prompt_status = report_prompt_status(&spawn.prompt_status);

    // Echo agent_id/agent_kind/cwd alongside the tmux name to match
    // `spawn-worker`'s output shape so orchestrators get a uniform envelope.
    print_json(&serde_json::json!({
        "ok": true,
        "agent_id": spawn.state.orchestrator_agent_id,
        "agent_kind": spawn.state.orchestrator_agent_kind,
        "cwd": spawn.state.project_root.to_string_lossy(),
        "tmux": spawn.tmux,
        "prompt_status": prompt_status,
        "task_id": task_id,
        "project_id": project_id,
    }));
    Ok(())
}

/// Seconds since `unix_secs`, clamped at 0 for clock skew.
fn age_secs(unix_secs: i64) -> u64 {
    (orchestrator::now_unix_secs() - unix_secs).max(0) as u64
}

/// Parse an optional `--status` flag into a `TaskStatus`, surfacing a uniform
/// usage error for unknown values. `None` flag → `Ok(None)`.
fn parse_status_flag(raw: Option<&str>) -> Result<Option<TaskStatus>, CliError> {
    match raw {
        None => Ok(None),
        Some(s) => s.parse::<TaskStatus>().map(Some).map_err(|_| {
            CliError::Usage(format!(
                "--status must be backlog|running|review|merging|done (got {})",
                s
            ))
        }),
    }
}

/// `cc-hub task list [--status STATUS] [--project-id ID] [--json]`
///
/// Enumerate tasks for a project by reading `~/.cc-hub/projects/<pid>/tasks/*/state.json`
/// directly — `projects_scan::scan` only sees projects registered in
/// `projects.toml`, so this verb works for ad-hoc/unregistered projects too.
fn task_list(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let project_id = resolve_project_id(&f)?;

    let filter = parse_status_flag(f.status.as_deref())?;

    let Some(project_dir) = orchestrator::project_state_dir(&project_id) else {
        return Err(CliError::Other("no home dir".into()));
    };
    let tasks_dir = project_dir.join("tasks");

    let mut tasks: Vec<TaskState> = Vec::new();
    match std::fs::read_dir(&tasks_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let task_id = name.to_string_lossy().into_owned();
                if !task_id.starts_with("t-") {
                    continue;
                }
                match orchestrator::read_task_state(&project_id, &task_id) {
                    Ok(state) => tasks.push(state),
                    Err(e) => {
                        eprintln!("warning: skipping {}: {}", task_id, e);
                    }
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(CliError::Other(format!(
                "read tasks dir {}: {}",
                tasks_dir.display(),
                e
            )));
        }
    }

    if let Some(ref s) = filter {
        tasks.retain(|t| &t.status == s);
    }

    tasks.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then(b.task_id.cmp(&a.task_id))
    });

    if f.json {
        let arr: Vec<serde_json::Value> = tasks
            .iter()
            .map(|t| {
                serde_json::json!({
                    "task_id": t.task_id,
                    "status": t.status,
                    "title": t.title,
                    "prompt": t.prompt,
                    "note": t.note,
                    "updated_at": t.updated_at,
                    "shipped_version": t.shipped_version,
                })
            })
            .collect();
        print_json(&serde_json::json!({ "ok": true, "tasks": arr }));
    } else {
        for t in &tasks {
            let preview = match t.title.as_deref() {
                Some(s) => s.to_string(),
                None => {
                    let first = t.prompt.lines().next().unwrap_or("").trim_end();
                    if first.chars().count() > 60 {
                        let truncated: String = first.chars().take(60).collect();
                        format!("{}…", truncated)
                    } else {
                        first.to_string()
                    }
                }
            };
            println!(
                "{}\t{}\t{}\t{}",
                t.task_id,
                t.status.as_str(),
                preview,
                models::relative_age_short(age_secs(t.updated_at))
            );
        }
    }

    Ok(())
}

fn task_report(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let raw_status = parse_status_flag(f.status.as_deref())?;

    let outcome = ops::task::task_report(
        &project_id,
        &task_id,
        ops::task::ReportOpts {
            status: raw_status,
            note: f.note.clone(),
            summary: f.summary.clone(),
        },
    )?;
    let state = outcome.state;

    print_json(&serde_json::json!({
        "ok": true,
        "task_id": state.task_id,
        "project_id": state.project_id,
        "status": state.status,
        "requested_status": outcome.requested_status,
        "note": state.note,
        "summary": state.summary,
        "shipped_version": state.shipped_version,
        "updated_at": state.updated_at,
    }));
    Ok(())
}

/// `cc-hub task show --task ID [--project-id ID] [--json]`
///
/// Read-only inspection of a single task. With `--json`, emits one JSON
/// object on stdout containing the full TaskState verbatim at the top level
/// plus `ok: true` and an embedded `pr` field (the PR object, or null).
/// Without `--json`, prints key/value lines suitable for skim-reading.
fn task_show(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let state = orchestrator::read_task_state(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("load state: {}", e)))?;
    let pr = cc_hub_lib::pr::read_pr(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("read pr: {}", e)))?;

    if f.json {
        // Top-level shape: full TaskState fields, plus `ok` and `pr`.
        let mut obj = serde_json::to_value(&state)
            .map_err(|e| CliError::Other(format!("serialize state: {}", e)))?;
        let map = obj
            .as_object_mut()
            .expect("TaskState serializes to a JSON object");
        map.insert("ok".into(), serde_json::Value::Bool(true));
        map.insert(
            "pr".into(),
            match &pr {
                Some(p) => pr_to_json(p),
                None => serde_json::Value::Null,
            },
        );
        print_json(&obj);
        return Ok(());
    }

    let todos_done = state.todos.iter().filter(|t| t.done).count();
    let todos_total = state.todos.len();

    println!("status: {}", state.status.as_str());
    println!(
        "prompt: {}",
        models::first_line_truncated(&state.prompt, 80)
    );
    println!("note: {}", state.note.as_deref().unwrap_or("-"));
    println!(
        "summary: {}",
        state
            .summary
            .as_deref()
            .map(|s| models::first_line_truncated(s, 120))
            .unwrap_or_else(|| "-".into())
    );
    println!(
        "created_at: {}",
        models::relative_age(age_secs(state.created_at))
    );
    println!(
        "updated_at: {}",
        models::relative_age(age_secs(state.updated_at))
    );
    println!("workers: {}", state.workers.len());
    println!("artifacts: {}", state.artifacts.len());
    println!("todos: {}/{}", todos_done, todos_total);
    println!(
        "shipped_version: {}",
        state.shipped_version.as_deref().unwrap_or("-")
    );
    println!(
        "orchestrator_tmux: {}",
        state.orchestrator_tmux.as_deref().unwrap_or("-")
    );
    Ok(())
}

/// `cc-hub task auto-review --task ID [--project-id ID]`
///
/// Re-arm the auto-reviewer for the current Review round by clearing
/// `last_auto_reviewed_at`. The next `auto_review::tick` will then re-pick
/// the task. Useful after fixing a misconfig (e.g. a stale agent setting)
/// when the user wants another pass without waiting for a fresh round.
fn task_auto_review(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    ops::task::task_auto_review(&project_id, &task_id)?;

    print_json(&serde_json::json!({
        "ok": true,
        "task_id": task_id,
        "project_id": project_id,
        "cleared": true,
    }));
    Ok(())
}

// ─── task artifact ───────────────────────────────────────────────────────

fn task_artifact_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args.split_first().ok_or_else(|| {
        CliError::Usage("task artifact <verb>: missing verb (try `add` or `list`)".into())
    })?;
    match verb.as_str() {
        "add" => task_artifact_add(rest),
        "list" => task_artifact_list(rest),
        other => Err(CliError::Usage(format!(
            "unknown task artifact verb: {} (try `add` or `list`)",
            other
        ))),
    }
}

fn task_artifact_add(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let raw_path = f
        .path
        .clone()
        .ok_or_else(|| CliError::Usage("--path is required".into()))?;

    let state = ops::task::task_artifact_add(
        &project_id,
        &task_id,
        &raw_path,
        f.kind.clone(),
        f.caption.clone(),
        f.lead,
    )?;

    let added_idx = state.artifacts.len() - 1;
    let added = state.artifacts.last().expect("just pushed");
    let is_lead = state.lead_artifact == Some(added_idx);
    print_json(&serde_json::json!({
        "ok": true,
        "task_id": state.task_id,
        "project_id": state.project_id,
        "artifact": {
            "kind": added.kind,
            "path": added.path,
            "original": added.original,
            "caption": added.caption,
            "added_at": added.added_at,
            "lead": is_lead,
        },
        "count": state.artifacts.len(),
        "lead_index": state.lead_artifact,
    }));
    Ok(())
}

fn task_artifact_list(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let state = ops::task::task_artifact_list(&project_id, &task_id)?;

    let lead_idx = state.lead_artifact;
    let arr: Vec<serde_json::Value> = state
        .artifacts
        .iter()
        .enumerate()
        .map(|(i, a)| {
            serde_json::json!({
                "kind": a.kind,
                "path": a.path,
                "original": a.original,
                "caption": a.caption,
                "added_at": a.added_at,
                "lead": lead_idx == Some(i),
            })
        })
        .collect();
    print_json(&serde_json::json!({ "ok": true, "artifacts": arr }));
    Ok(())
}

// ─── task todos ──────────────────────────────────────────────────────────

fn task_todos_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args.split_first().ok_or_else(|| {
        CliError::Usage(
            "task todos <verb>: missing verb (try `set`, `check`, `uncheck`, or `clear`)".into(),
        )
    })?;
    match verb.as_str() {
        "set" => task_todos_set(rest),
        "check" => task_todos_mark(rest, true),
        "uncheck" => task_todos_mark(rest, false),
        "clear" => task_todos_clear(rest),
        other => Err(CliError::Usage(format!(
            "unknown task todos verb: {} (try `set`, `check`, `uncheck`, or `clear`)",
            other
        ))),
    }
}

fn todos_to_json(todos: &[TodoItem]) -> Vec<serde_json::Value> {
    todos
        .iter()
        .map(|t| serde_json::json!({ "text": t.text, "done": t.done }))
        .collect()
}

fn print_todos_result(state: &TaskState) {
    print_json(&serde_json::json!({
        "ok": true,
        "task_id": state.task_id,
        "project_id": state.project_id,
        "todos": todos_to_json(&state.todos),
    }));
}

fn task_todos_set(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let raw = f.items.clone().ok_or_else(|| {
        CliError::Usage("--items is required (newline-separated todo texts)".into())
    })?;

    let state = ops::task::task_todos_set(&project_id, &task_id, &raw)?;

    print_todos_result(&state);
    Ok(())
}

fn task_todos_mark(args: &[String], done: bool) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let idx = f
        .index
        .ok_or_else(|| CliError::Usage("--index is required (0-based)".into()))?;

    let state = ops::task::task_todos_mark(&project_id, &task_id, idx, done)?;

    print_todos_result(&state);
    Ok(())
}

fn task_todos_clear(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let state = ops::task::task_todos_clear(&project_id, &task_id)?;

    print_todos_result(&state);
    Ok(())
}

/// `cc-hub task create --prompt "..." [--project-id ID] [--name NAME]`
///
/// Headless task creation — used by tests and tooling that wants to seed a
/// task without going through the TUI's `N → folder → prompt` flow.
fn task_create(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let name = f.name.clone();
    let prompt = f
        .prompt
        .clone()
        .ok_or_else(|| CliError::Usage("--prompt is required".into()))?;

    let state = ops::task::task_create(f.project_id.as_deref(), name, prompt, f.backlog)?;

    print_json(&serde_json::json!({
        "ok": true,
        "task_id": state.task_id,
        "project_id": state.project_id,
        "status": state.status,
    }));
    Ok(())
}

// ─── pr ──────────────────────────────────────────────────────────────────
//
// PR-flow primitives. The orchestrator opens a PR with `pr create`, the user
// reviews in the TUI (or via `pr approve` / `pr request-changes` from the
// CLI), and merging is serialized through the per-project merge lock:
//
//   pr create   → task: Running → Review,  PR: (new, Open)
//   pr request-changes → task: Review → Running, PR: ChangesRequested
//   pr reopen   → task: Running → Review, PR: ChangesRequested → Open
//   pr approve  → PR: Open|ChangesRequested → Approved (task stays Review)
//   pr merge    → acquires merge.lock, merges main → branch then branch →
//                 main; lock stays held; task: Review → Merging
//   pr finalize → releases merge.lock; task: Merging → Done; PR: Merged
//
// `pr show` is a read-only convenience for inspection / scripting.

fn pr_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args.split_first().ok_or_else(|| {
        CliError::Usage(
            "pr <verb>: missing verb (try `create`, `show`, `approve`, `request-changes`, `reopen`, `comment`, `close`, `merge`, `continue`, `lock-phase`, `finalize`)".into(),
        )
    })?;
    match verb.as_str() {
        "create" => pr_create(rest),
        "show" => pr_show(rest),
        "approve" => pr_approve(rest),
        "request-changes" => pr_request_changes(rest),
        "reopen" => pr_reopen(rest),
        "comment" => pr_comment(rest),
        "close" => pr_close(rest),
        "merge" => pr_merge(rest),
        "continue" => pr_continue(rest),
        "lock-phase" => pr_lock_phase(rest),
        "finalize" => pr_finalize(rest),
        other => Err(CliError::Usage(format!(
            "unknown pr verb: {} (try `create`, `show`, `approve`, `request-changes`, `reopen`, `comment`, `close`, `merge`, `continue`, `lock-phase`, `finalize`)",
            other
        ))),
    }
}

fn pr_to_json(pr: &cc_hub_lib::pr::PullRequest) -> serde_json::Value {
    pr_to_json_filtered(pr, None)
}

fn pr_to_json_filtered(
    pr: &cc_hub_lib::pr::PullRequest,
    comments_since: Option<i64>,
) -> serde_json::Value {
    let total = pr.comments.len();
    let (comments, returned): (serde_json::Value, usize) = match comments_since {
        Some(threshold) => {
            let filtered: Vec<&cc_hub_lib::pr::Comment> =
                pr.comments.iter().filter(|c| c.at >= threshold).collect();
            let len = filtered.len();
            (serde_json::json!(filtered), len)
        }
        None => (serde_json::json!(pr.comments), total),
    };
    serde_json::json!({
        "id": pr.id,
        "task_id": pr.task_id,
        "project_id": pr.project_id,
        "branch": pr.branch,
        "base": pr.base,
        "title": pr.title,
        "description": pr.description,
        "review_state": pr.review_state.as_str(),
        "comments": comments,
        "comments_total": total,
        "comments_returned": returned,
        "approved_at_branch_sha": pr.approved_at_branch_sha,
        "approved_at_base_sha": pr.approved_at_base_sha,
        "created_at": pr.created_at,
        "updated_at": pr.updated_at,
    })
}

fn pr_create(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let worktree_name = f
        .worktree
        .clone()
        .ok_or_else(|| CliError::Usage("--worktree NAME is required".into()))?;
    let title = f
        .title
        .clone()
        .ok_or_else(|| CliError::Usage("--title is required".into()))?;
    let description = f.description.clone().unwrap_or_default();

    let pr = ops::pr::pr_create(&project_id, &task_id, &worktree_name, title, description)?;

    print_json(&serde_json::json!({
        "ok": true,
        "pr": pr_to_json(&pr),
    }));
    Ok(())
}

fn pr_show(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let pr = cc_hub_lib::pr::read_pr(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("read pr: {}", e)))?
        .ok_or_else(|| CliError::NotFound("no PR for this task".into()))?;
    print_json(&serde_json::json!({
        "ok": true,
        "pr": pr_to_json_filtered(&pr, f.comments_since),
    }));
    Ok(())
}

fn pr_approve(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let pr = ops::pr::pr_approve(&project_id, &task_id)?;

    print_json(&serde_json::json!({
        "ok": true,
        "pr": pr_to_json(&pr),
    }));
    Ok(())
}

fn pr_request_changes(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let comment = f
        .comment
        .clone()
        .ok_or_else(|| CliError::Usage("--comment is required".into()))?;
    let author = f.author.clone().unwrap_or_else(|| "user".into());

    let pr = ops::pr::pr_request_changes(&project_id, &task_id, comment, author)?;

    print_json(&serde_json::json!({
        "ok": true,
        "pr": pr_to_json(&pr),
    }));
    Ok(())
}

fn pr_reopen(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let comment = f.comment.clone();
    let author = f.author.clone().unwrap_or_else(|| "orchestrator".into());

    let pr = ops::pr::pr_reopen(&project_id, &task_id, comment, author)?;

    print_json(&serde_json::json!({
        "ok": true,
        "pr": pr_to_json(&pr),
    }));
    Ok(())
}

fn pr_comment(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let body = f
        .comment
        .clone()
        .ok_or_else(|| CliError::Usage("--comment is required".into()))?;
    let author = f.author.clone().unwrap_or_else(|| "orchestrator".into());

    let pr = ops::pr::pr_comment(&project_id, &task_id, body, author)?;

    print_json(&serde_json::json!({
        "ok": true,
        "pr": pr_to_json(&pr),
    }));
    Ok(())
}

/// Non-destructive "abandon" path: flip the PR to Closed, mark the task Done,
/// drop the merge lock if held, and tear down sessions. Preserves the review
/// record (comments + history) instead of deleting the task.
fn pr_close(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let comment = f.comment.clone();
    let author = f.author.clone().unwrap_or_else(|| "user".into());

    let pr = ops::pr::pr_close(&project_id, &task_id, comment, author)?;

    print_json(&serde_json::json!({
        "ok": true,
        "pr": pr_to_json(&pr),
        "status": "done",
    }));
    Ok(())
}

fn pr_merge(args: &[String]) -> Result<(), CliError> {
    use ops::pr::MergeOutcomeOp;

    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let outcome = ops::pr::pr_merge(&project_id, &task_id, f.wait, f.timeout_secs)?;

    match outcome {
        MergeOutcomeOp::Locked(holder) => {
            let mut payload = serde_json::json!({
                "ok": false,
                "locked": true,
                "holder_task": holder.holder_task,
                "since": holder.since,
                "phase": holder.phase,
                "age_seconds": holder.age_seconds,
                "recipe": "Another task currently holds the merge lock. Re-run with `--wait` to block until it releases, or poll `cc-hub pr merge` manually.",
            });
            if holder.timed_out {
                payload["timed_out"] = serde_json::Value::Bool(true);
                payload["recipe"] = serde_json::Value::String(
                    "Merge lock still held after the wait timeout. Re-run `cc-hub pr merge --wait` (optionally with `--timeout-secs N`) or investigate why the holder is stuck.".into(),
                );
            }
            print_json(&payload);
            Err(CliError::Reported(format!(
                "merge lock held by task {}",
                holder.holder_task
            )))
        }
        MergeOutcomeOp::MissingWorktree { worktree, branch } => {
            print_json(&serde_json::json!({
                "ok": false,
                "phase": "preflight",
                "kind": "missing_worktree",
                "worktree": worktree,
                "recipe": "The feature branch worktree no longer exists (removed by gc or manual cleanup). Recreate it (`cc-hub spawn-worker`) on the same branch before re-running `cc-hub pr merge`, or close the PR. The merge lock has been released.",
            }));
            Err(CliError::Reported(format!(
                "worktree for branch {} no longer exists",
                branch
            )))
        }
        MergeOutcomeOp::DirtyWorktree { worktree, dirty } => {
            let dirty_len = dirty.len();
            print_json(&serde_json::json!({
                "ok": false,
                "phase": "preflight",
                "kind": "dirty_worktree",
                "worktree": worktree,
                "dirty": dirty,
                "recipe": "The feature-branch worktree has uncommitted changes; cc-hub won't merge base into a dirty tree. Commit or stash the listed paths in the worktree, then re-run `cc-hub pr merge`. The merge lock has been released.",
            }));
            Err(CliError::Reported(format!(
                "merge blocked: feature-branch worktree has {} uncommitted path(s)",
                dirty_len
            )))
        }
        MergeOutcomeOp::DemotedConflict {
            conflicting_paths,
            stdout,
            stderr,
        } => {
            print_json(&serde_json::json!({
                "ok": false,
                "phase": "merge_main_into_branch",
                "demoted_to": "open",
                "conflicting_paths": conflicting_paths,
                "stdout": stdout,
                "stderr": stderr,
                "recipe": "Resolve conflicts in the worktree, commit the resolution, then ask the reviewer to re-approve before re-running `cc-hub pr merge`. The merge lock has been released.",
            }));
            Err(CliError::Reported(
                "conflict merging main into the feature branch — PR demoted to Open".into(),
            ))
        }
        MergeOutcomeOp::BlockedByDirtyTree { overlap } => {
            print_json(&serde_json::json!({
                "ok": false,
                "phase": "preflight",
                "blocked_by_dirty_tree": true,
                "overlap": overlap,
                "recipe": "Commit, stash, or revert the listed paths on the target branch, then re-run `cc-hub pr merge`. The merge lock has been released.",
            }));
            Err(CliError::Reported(
                "merge blocked: working tree on target branch has overlapping uncommitted edits"
                    .into(),
            ))
        }
        MergeOutcomeOp::ConflictIntoMain {
            conflicting_paths,
            stdout,
            stderr,
        } => {
            print_json(&serde_json::json!({
                "ok": false,
                "phase": "merge_branch_into_main",
                "conflicting_paths": conflicting_paths,
                "stdout": stdout,
                "stderr": stderr,
                "recipe": "Unexpected conflict merging into main (the merge lock should have prevented this — investigate before retrying).",
            }));
            Err(CliError::Reported("conflict merging into main".into()))
        }
        MergeOutcomeOp::Merged {
            branch,
            base,
            stdout,
            restored_ref,
        } => {
            print_json(&serde_json::json!({
                "ok": true,
                "phase": "merged",
                "branch": branch,
                "base": base,
                "stdout": stdout,
                "restored_ref": restored_ref,
                "next": "Run /simplify, then /bump, then `cc-hub pr finalize --task <id>` to release the merge lock and mark the task done.",
            }));
            Ok(())
        }
    }
}

/// `cc-hub pr continue --task ID [--project-id ID]`
///
/// Recovery / re-ping verb. Re-sends [`build_review_approval_prompt`] (the
/// same merge-flow nudge the TUI dispatches when a human approves a PR) to the
/// task's orchestrator. The common failure mode this recovers from: the user
/// approved a PR but the orchestrator never picked up the merge (it was busy,
/// the notify was dropped, or the session was restarted).
///
/// Idempotent — re-running just re-pings; nothing destructive happens. When
/// the orchestrator session is dead it returns
/// `{ok:false, orchestrator_alive:false}` with a recipe telling the user to
/// resurrect the orchestrator or `task delete --force` the wedged task. When
/// the pane is busy the prompt cannot be injected; the verb reports that
/// (`sent:false, pane_busy:true`) so the caller knows to retry once idle.
fn pr_continue(args: &[String]) -> Result<(), CliError> {
    use ops::pr::ContinueOutcome;

    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    match ops::pr::pr_continue(&project_id, &task_id)? {
        ContinueOutcome::NoOrchestratorTmux => {
            print_json(&serde_json::json!({
                "ok": false,
                "task_id": task_id,
                "orchestrator_alive": false,
                "reason": "no_orchestrator_tmux",
                "recipe": "This task has no orchestrator tmux recorded — it was never started or its session name was cleared. Resurrect the orchestrator (`cc-hub orchestrate start --task <id>`), or if the merge is wedged, `cc-hub task delete --force --task <id>` to recover.",
            }));
            Err(CliError::Reported(format!(
                "task {} has no orchestrator tmux to continue",
                task_id
            )))
        }
        ContinueOutcome::OrchestratorDead { tmux } => {
            print_json(&serde_json::json!({
                "ok": false,
                "task_id": task_id,
                "orchestrator_tmux": tmux,
                "orchestrator_alive": false,
                "reason": "orchestrator_dead",
                "recipe": "The orchestrator session is dead. Resurrect it (`cc-hub orchestrate start --task <id>`) then re-run `cc-hub pr continue`, or `cc-hub task delete --force --task <id>` to tear down a wedged merge (releases the merge lock).",
            }));
            Err(CliError::Reported(format!(
                "orchestrator [{}] is not live",
                tmux
            )))
        }
        ContinueOutcome::PaneBusy { tmux } => {
            print_json(&serde_json::json!({
                "ok": true,
                "task_id": task_id,
                "orchestrator_tmux": tmux,
                "orchestrator_alive": true,
                "sent": false,
                "pane_busy": true,
                "recipe": "The orchestrator is alive but its pane is busy (mid-turn). Re-run `cc-hub pr continue` once it's idle.",
            }));
            Ok(())
        }
        ContinueOutcome::Sent { tmux } => {
            print_json(&serde_json::json!({
                "ok": true,
                "task_id": task_id,
                "orchestrator_tmux": tmux,
                "orchestrator_alive": true,
                "sent": true,
            }));
            Ok(())
        }
    }
}

fn pr_lock_phase(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let phase_raw = f.phase.clone().ok_or_else(|| {
        CliError::Usage("--phase is required (merging|simplify|bump|finalize-pending)".into())
    })?;
    let phase = cc_hub_lib::merge_lock::MergePhase::parse(&phase_raw).ok_or_else(|| {
        CliError::Usage(format!(
            "--phase: unknown value '{}' (expected merging|simplify|bump|finalize-pending)",
            phase_raw
        ))
    })?;
    let phase_str = ops::pr::pr_lock_phase(&project_id, &task_id, phase)?;
    print_json(&serde_json::json!({
        "ok": true,
        "task_id": task_id,
        "project_id": project_id,
        "phase": phase_str,
    }));
    Ok(())
}

/// After the build gate passes, release the merge lock BEFORE flipping the PR
/// and task to terminal states. If the release fails the task stays in
/// `Merging` so a re-run can complete the transition — otherwise a transient
/// FS error would strand a `Done` task as the lock holder and block the
/// project's merge queue until `STALE_TTL_SECS`.
fn pr_finalize(args: &[String]) -> Result<(), CliError> {
    use ops::pr::FinalizeOutcome;

    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let outcome = ops::pr::pr_finalize(
        &project_id,
        &task_id,
        ops::pr::FinalizeOpts {
            build_cmd: f.build_cmd.clone(),
            skip_build: f.skip_build,
            keep_tmux: f.keep_tmux,
        },
    )?;

    match outcome {
        FinalizeOutcome::AlreadyMerged => {
            print_json(&serde_json::json!({
                "ok": true,
                "noop": true,
                "task_id": task_id,
                "status": "done",
                "reason": "pr already merged",
            }));
            Ok(())
        }
        FinalizeOutcome::BuildFailed {
            command,
            stderr_tail,
        } => {
            print_json(&serde_json::json!({
                "ok": false,
                "phase": "build",
                "command": command,
                "stderr": stderr_tail,
                "recipe": "Build failed on main after /simplify or /bump; fix in the working tree, commit, then re-run cc-hub pr finalize.",
            }));
            Err(CliError::Reported("build gate failed".into()))
        }
        FinalizeOutcome::Finalized {
            released,
            build_skipped,
            tmux_kept,
        } => {
            print_json(&serde_json::json!({
                "ok": true,
                "released": released,
                "task_id": task_id,
                "status": "done",
                "build_skipped": build_skipped,
                "tmux_kept": tmux_kept,
            }));
            Ok(())
        }
    }
}

// ─── worker ──────────────────────────────────────────────────────────────
//
// `cc-hub worker wait --task ID [--tmux NAME ...] [--worktree NAME ...] [--all] [--timeout-secs N]`
//
// Blocks until the named worker tmux session(s) reach a terminal-for-the-
// orchestrator state — WaitingForInput (Claude end_turn), Question (agent
// blocked on AskUserQuestion), or Inactive (process gone). Replaces the
// orchestrator's tmux capture-pane polling loop, which paid 60–90s of
// LLM-driven latency per spawn; this verb polls scan_sessions() at 500 ms
// and returns within seconds.

fn worker_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args
        .split_first()
        .ok_or_else(|| CliError::Usage("worker <verb>: missing verb (try `wait`)".into()))?;
    match verb.as_str() {
        "wait" => worker_wait(rest),
        other => Err(CliError::Usage(format!(
            "unknown worker verb: {} (try `wait`)",
            other
        ))),
    }
}

/// Resolve the tmux-name targets for `worker wait` from the various
/// selection flags. Returns a deduped Vec of tmux session names.
///
/// Selection is the union of:
///   * each `--tmux NAME`  (must exist in state.workers as a tmux_name)
///   * each `--worktree NAME` (must exist in state.workers as a worktree)
///   * `--all` (every worker on the task)
///
/// Passing none of the three is a usage error. `--all` on a task with
/// zero workers yields an empty Vec (preserved existing behavior — the
/// caller emits ok=true / all_done=true / workers=[]).
fn resolve_wait_targets(
    state: &orchestrator::TaskState,
    tmux_targets: &[String],
    worktree_targets: &[String],
    all: bool,
) -> Result<Vec<String>, CliError> {
    Ok(ops::worker::resolve_wait_targets(
        state,
        tmux_targets,
        worktree_targets,
        all,
    )?)
}

fn worker_wait(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let state = orchestrator::read_task_state(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("load state: {}", e)))?;

    let targets = resolve_wait_targets(&state, &f.tmux_targets, &f.worktree_targets, f.all)?;

    if targets.is_empty() {
        print_json(&serde_json::json!({
            "ok": true,
            "all_done": true,
            "timed_out": false,
            "elapsed_secs": 0,
            "workers": [],
        }));
        return Ok(());
    }

    let timeout = Duration::from_secs(f.timeout_secs.unwrap_or(1800));

    let progress_interval = if f.progress {
        Some(Duration::from_secs(
            f.progress_interval_secs.unwrap_or(5).max(1),
        ))
    } else {
        None
    };

    let outcome = ops::worker::worker_wait(&targets, timeout, progress_interval, |p| {
        print_json(&serde_json::json!({
            "event": "progress",
            "elapsed_secs": p.elapsed_secs,
            "pending": p.pending,
            "done": p.done,
        }));
    });

    print_json(&serde_json::json!({
        "ok": true,
        "all_done": outcome.all_done,
        "timed_out": outcome.timed_out,
        "elapsed_secs": outcome.elapsed_secs,
        "workers": outcome.workers,
    }));
    Ok(())
}

fn project_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args
        .split_first()
        .ok_or_else(|| CliError::Usage("project <verb>: missing verb (try `list`)".into()))?;
    match verb.as_str() {
        "list" => project_list(rest),
        other => Err(CliError::Usage(format!(
            "unknown project verb: {} (try `list`)",
            other
        ))),
    }
}

/// `cc-hub project list [--json]`
///
/// Enumerate registered projects from `~/.cc-hub/projects.toml`. Plain
/// output is one tab-separated row per project: `<id>\t<name>\t<root>`.
/// With `--json`, one `{ok:true, projects:[{id, name, root,
/// task_counts:{backlog,running,review,merging,done}}]}` envelope. Sorted by
/// name (case-insensitive) so the listing is stable across machines.
fn project_list(args: &[String]) -> Result<(), CliError> {
    use cc_hub_lib::orchestrator::TaskStatus;
    use cc_hub_lib::projects_scan;

    let f = parse_flags(args)?;
    let mut snap = projects_scan::scan();

    let mut projects = std::mem::take(&mut snap.projects);
    projects.sort_by_key(|a| a.name.to_lowercase());

    if f.json {
        let arr: Vec<serde_json::Value> = projects
            .iter()
            .map(|p| {
                let tasks = snap.tasks.get(&p.id).map(|v| v.as_slice()).unwrap_or(&[]);
                let mut backlog = 0usize;
                let mut running = 0usize;
                let mut review = 0usize;
                let mut merging = 0usize;
                let mut done = 0usize;
                for t in tasks {
                    match t.status {
                        TaskStatus::Backlog => backlog += 1,
                        TaskStatus::Running => running += 1,
                        TaskStatus::Review => review += 1,
                        TaskStatus::Merging => merging += 1,
                        TaskStatus::Done => done += 1,
                    }
                }
                serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    "root": p.root,
                    "task_counts": {
                        "backlog": backlog,
                        "running": running,
                        "review": review,
                        "merging": merging,
                        "done": done,
                    },
                })
            })
            .collect();
        print_json(&serde_json::json!({ "ok": true, "projects": arr }));
    } else {
        // Tab-separated so consumers can split on \t even if a name contains spaces.
        for p in &projects {
            println!("{}\t{}\t{}", p.id, p.name, p.root.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_hub_lib::pr;

    fn with_tempdir_home<F: FnOnce()>(f: F) {
        // $HOME / CLAUDE_CONFIG_DIR are process-global; serialise on the
        // crate-wide lock so these tests don't race env-mutating tests in
        // other modules (e.g. `extract_claude_config_dir`'s).
        let _g = crate::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        f();
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn dispatch_handles_help() {
        assert_eq!(dispatch(&["--help".to_string()]), Some(0));
        assert_eq!(
            dispatch(&["task".to_string(), "--help".to_string()]),
            Some(0)
        );
    }

    #[test]
    fn dispatch_parses_project_list_json() {
        let argv = vec![
            "project".to_string(),
            "list".to_string(),
            "--json".to_string(),
        ];
        let code = dispatch(&argv);
        assert_eq!(
            code,
            Some(0),
            "expected dispatch to handle 'project list --json' cleanly"
        );
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

    #[test]
    fn auto_review_clears_timestamp_on_review_with_open_pr() {
        with_tempdir_home(|| {
            let project_id = "p1".to_string();
            let task_id = "t-auto".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Review;
            state.last_auto_reviewed_at = Some(123_456);
            orchestrator::write_task_state(&state).expect("write state");

            let created = pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");
            assert_eq!(created.review_state, pr::ReviewState::Open);

            let args = vec![
                "--task".to_string(),
                task_id.clone(),
                "--project-id".to_string(),
                project_id.clone(),
            ];
            task_auto_review(&args).expect("auto-review ok");

            let after = orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert!(after.last_auto_reviewed_at.is_none());
        });
    }

    #[test]
    fn pr_finalize_build_failure_keeps_lock_and_comments_pr() {
        with_tempdir_home(|| {
            let project_id = "p-build".to_string();
            let task_id = "t-build".to_string();

            // Project root lives outside $HOME so we can drop a build script in it.
            let project_dir = tempfile::tempdir().expect("project tempdir");
            let project_root = project_dir.path().to_path_buf();

            // State: task is Merging, /simplify + /bump nominally complete.
            let mut state =
                TaskState::new(project_id.clone(), project_root.clone(), "do thing".into());
            state.task_id = task_id.clone();
            state.status = TaskStatus::Merging;
            orchestrator::write_task_state(&state).expect("write state");

            // Lock held by this task so we can observe it stays held on failure.
            cc_hub_lib::merge_lock::acquire(&project_id, &task_id, None)
                .expect("acquire merge lock");

            // PR exists in Approved state.
            let pr_record = pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");
            pr::update_pr(&project_id, &task_id, |p| {
                p.review_state = pr::ReviewState::Approved;
            })
            .expect("approve pr");
            assert_eq!(pr_record.review_state, pr::ReviewState::Open);

            // Build script that fails with a known stderr signature.
            let build_script = project_root.join("build.sh");
            std::fs::write(
                &build_script,
                "#!/bin/sh\necho 'build broke!' 1>&2\nexit 7\n",
            )
            .expect("write build script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&build_script).unwrap().permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&build_script, perms).unwrap();
            }

            let result = pr_finalize(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--build-cmd".into(),
                build_script.to_string_lossy().into_owned(),
            ]);
            match result {
                Err(CliError::Reported(msg)) => assert!(
                    msg.contains("build gate failed"),
                    "unexpected error: {}",
                    msg
                ),
                other => panic!("expected build gate failure, got {:?}", other),
            }

            // Task stayed in Merging.
            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Merging);

            // PR did NOT flip to Merged; it stays Approved with a new comment.
            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Approved);
            assert!(
                after_pr
                    .comments
                    .iter()
                    .any(|c| c.body.contains("build broke!")),
                "expected PR comment containing build stderr; got {:?}",
                after_pr.comments,
            );

            // Merge lock still held.
            let holder = cc_hub_lib::merge_lock::current_holder(&project_id)
                .expect("read lock")
                .expect("lock still held");
            assert_eq!(holder.task_id, task_id);
        });
    }

    #[test]
    fn pr_finalize_skip_build_releases_lock_even_with_missing_command() {
        with_tempdir_home(|| {
            let project_id = "p-skip".to_string();
            let task_id = "t-skip".to_string();
            let project_dir = tempfile::tempdir().expect("project tempdir");

            let mut state = TaskState::new(
                project_id.clone(),
                project_dir.path().to_path_buf(),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Merging;
            orchestrator::write_task_state(&state).expect("write state");

            cc_hub_lib::merge_lock::acquire(&project_id, &task_id, None).expect("acquire");
            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");

            pr_finalize(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--skip-build".into(),
                "--build-cmd".into(),
                "/nonexistent/build-script-does-not-exist".into(),
            ])
            .expect("finalize ok with --skip-build");

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Done);

            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Merged);

            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("read lock")
                    .is_none(),
                "lock should be released after successful finalize"
            );
        });
    }

    #[test]
    fn pr_finalize_keep_tmux_still_marks_task_done() {
        with_tempdir_home(|| {
            let project_id = "p-keep".to_string();
            let task_id = "t-keep".to_string();
            let project_dir = tempfile::tempdir().expect("project tempdir");

            let mut state = TaskState::new(
                project_id.clone(),
                project_dir.path().to_path_buf(),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Merging;
            state.orchestrator_tmux = Some("cc-hub-test-orch".into());
            orchestrator::write_task_state(&state).expect("write state");

            cc_hub_lib::merge_lock::acquire(&project_id, &task_id, None).expect("acquire");
            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");

            pr_finalize(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--skip-build".into(),
                "--keep-tmux".into(),
            ])
            .expect("finalize ok with --keep-tmux");

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Done);
            // We don't observe tmux state in unit tests (cleanup happens via
            // detached `sh -c sleep …; tmux …`). Asserting Done + Merged is
            // the user-visible signal; whether cleanup_task_sessions ran is
            // an integration concern.
        });
    }

    #[test]
    fn pr_finalize_refuses_when_task_not_merging() {
        with_tempdir_home(|| {
            let project_id = "p-guard-review".to_string();
            let task_id = "t-guard-review".to_string();
            let project_dir = tempfile::tempdir().expect("project tempdir");

            let mut state = TaskState::new(
                project_id.clone(),
                project_dir.path().to_path_buf(),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Review;
            orchestrator::write_task_state(&state).expect("write state");

            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");
            pr::update_pr(&project_id, &task_id, |p| {
                p.review_state = pr::ReviewState::Approved;
            })
            .expect("approve pr");

            let result = pr_finalize(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--skip-build".into(),
            ]);
            match result {
                Err(CliError::Usage(msg)) => {
                    assert!(msg.contains("Merging"), "expected Merging in msg: {}", msg);
                    assert!(
                        msg.contains("cc-hub pr merge"),
                        "expected `cc-hub pr merge` in msg: {}",
                        msg
                    );
                }
                other => panic!("expected Usage error, got {:?}", other),
            }

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Review);

            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Approved);
        });
    }

    #[test]
    fn pr_finalize_refuses_when_no_pr_exists() {
        with_tempdir_home(|| {
            let project_id = "p-guard-nopr".to_string();
            let task_id = "t-guard-nopr".to_string();
            let project_dir = tempfile::tempdir().expect("project tempdir");

            let mut state = TaskState::new(
                project_id.clone(),
                project_dir.path().to_path_buf(),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Merging;
            orchestrator::write_task_state(&state).expect("write state");

            cc_hub_lib::merge_lock::acquire(&project_id, &task_id, None).expect("acquire lock");

            let result = pr_finalize(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--skip-build".into(),
            ]);
            match result {
                Err(CliError::Usage(msg)) => {
                    assert!(msg.contains("no PR"), "expected `no PR` in msg: {}", msg);
                }
                other => panic!("expected Usage error, got {:?}", other),
            }

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Merging);

            let holder = cc_hub_lib::merge_lock::current_holder(&project_id)
                .expect("read lock")
                .expect("lock still held");
            assert_eq!(holder.task_id, task_id);
        });
    }

    #[test]
    fn pr_finalize_is_idempotent_when_pr_already_merged() {
        with_tempdir_home(|| {
            let project_id = "p-guard-merged".to_string();
            let task_id = "t-guard-merged".to_string();
            let project_dir = tempfile::tempdir().expect("project tempdir");

            let mut state = TaskState::new(
                project_id.clone(),
                project_dir.path().to_path_buf(),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Done;
            orchestrator::write_task_state(&state).expect("write state");

            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");
            pr::update_pr(&project_id, &task_id, |p| {
                p.review_state = pr::ReviewState::Merged;
            })
            .expect("merge pr");

            pr_finalize(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--skip-build".into(),
            ])
            .expect("idempotent finalize returns Ok");

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Done);

            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Merged);

            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("read lock")
                    .is_none(),
                "lock should remain unheld on idempotent retry"
            );
        });
    }

    #[test]
    fn pr_finalize_refuses_when_task_backlog() {
        with_tempdir_home(|| {
            let project_id = "p-guard-backlog".to_string();
            let task_id = "t-guard-backlog".to_string();
            let project_dir = tempfile::tempdir().expect("project tempdir");

            let mut state = TaskState::new_backlog(
                project_id.clone(),
                project_dir.path().to_path_buf(),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            assert_eq!(state.status, TaskStatus::Backlog);
            orchestrator::write_task_state(&state).expect("write state");

            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");

            let result = pr_finalize(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--skip-build".into(),
            ]);
            match result {
                Err(CliError::Usage(msg)) => {
                    assert!(msg.contains("Merging"), "expected Merging in msg: {}", msg);
                    assert!(
                        msg.contains("cc-hub pr merge"),
                        "expected `cc-hub pr merge` in msg: {}",
                        msg
                    );
                }
                other => panic!("expected Usage error, got {:?}", other),
            }

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Backlog);

            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Open);

            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("read lock")
                    .is_none(),
                "lock should remain unheld when finalize is refused"
            );
        });
    }

    #[test]
    fn pr_close_from_open_marks_task_done() {
        with_tempdir_home(|| {
            let project_id = "p-close-open".to_string();
            let task_id = "t-close-open".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Review;
            orchestrator::write_task_state(&state).expect("write state");

            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");

            // Hold the merge lock so we can assert release on close.
            cc_hub_lib::merge_lock::acquire(&project_id, &task_id, None).expect("acquire");

            pr_close(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--comment".into(),
                "abandoning this work".into(),
            ])
            .expect("pr close ok");

            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Closed);
            let last = after_pr.comments.last().expect("comment appended");
            assert_eq!(last.author, "user");
            assert_eq!(last.body, "abandoning this work");

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Done);
            assert!(
                after_state.note.as_deref().unwrap_or("").contains("closed"),
                "note should mention closed, got {:?}",
                after_state.note
            );

            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("read lock")
                    .is_none(),
                "lock should be released after close"
            );
        });
    }

    #[test]
    fn pr_close_from_changes_requested_works() {
        with_tempdir_home(|| {
            let project_id = "p-close-cr".to_string();
            let task_id = "t-close-cr".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Running;
            orchestrator::write_task_state(&state).expect("write state");

            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");
            pr::update_pr(&project_id, &task_id, |p| {
                p.review_state = pr::ReviewState::ChangesRequested;
            })
            .expect("set changes_requested");

            pr_close(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
            ])
            .expect("pr close ok");

            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Closed);

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Done);
        });
    }

    #[test]
    fn pr_close_refuses_merged_and_closed() {
        with_tempdir_home(|| {
            let project_id = "p-close-refuse".to_string();
            let task_id = "t-close-refuse".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Done;
            orchestrator::write_task_state(&state).expect("write state");

            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");

            // Already-merged PR: refuse.
            pr::update_pr(&project_id, &task_id, |p| {
                p.review_state = pr::ReviewState::Merged;
            })
            .expect("set merged");
            let err = pr_close(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
            ])
            .expect_err("close on merged should fail");
            match err {
                CliError::Usage(msg) => assert!(
                    msg.contains("merged"),
                    "expected message mentioning merged, got: {msg}"
                ),
                other => panic!("expected CliError::Usage, got {other:?}"),
            }

            // Already-closed PR: refuse.
            pr::update_pr(&project_id, &task_id, |p| {
                p.review_state = pr::ReviewState::Closed;
            })
            .expect("set closed");
            let err = pr_close(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
            ])
            .expect_err("close on closed should fail");
            match err {
                CliError::Usage(msg) => assert!(
                    msg.contains("closed"),
                    "expected message mentioning closed, got: {msg}"
                ),
                other => panic!("expected CliError::Usage, got {other:?}"),
            }
        });
    }

    #[test]
    fn pr_show_comments_since_filters_old_comments() {
        let pr = pr::PullRequest {
            id: 1,
            task_id: "t-x".into(),
            project_id: "p1".into(),
            branch: "feature".into(),
            base: "main".into(),
            title: "title".into(),
            description: "desc".into(),
            review_state: pr::ReviewState::Open,
            comments: vec![
                pr::Comment {
                    author: "user".into(),
                    at: 100,
                    body: "first".into(),
                },
                pr::Comment {
                    author: "user".into(),
                    at: 200,
                    body: "second".into(),
                },
                pr::Comment {
                    author: "user".into(),
                    at: 300,
                    body: "third".into(),
                },
            ],
            approved_at_branch_sha: None,
            approved_at_base_sha: None,
            created_at: 0,
            updated_at: 300,
        };

        let none = pr_to_json_filtered(&pr, None);
        assert_eq!(none["comments"].as_array().expect("array").len(), 3);
        assert_eq!(none["comments_total"], 3);
        assert_eq!(none["comments_returned"], 3);

        let since_200 = pr_to_json_filtered(&pr, Some(200));
        let kept = since_200["comments"].as_array().expect("array");
        assert_eq!(kept.len(), 2);
        assert_eq!(since_200["comments_total"], 3);
        assert_eq!(since_200["comments_returned"], 2);
        let stamps: Vec<i64> = kept.iter().map(|c| c["at"].as_i64().unwrap()).collect();
        assert_eq!(stamps, vec![200, 300]);

        let since_999 = pr_to_json_filtered(&pr, Some(999));
        assert_eq!(since_999["comments"].as_array().expect("array").len(), 0);
        assert_eq!(since_999["comments_total"], 3);
        assert_eq!(since_999["comments_returned"], 0);
    }

    #[test]
    fn task_report_rejects_backlog_to_running_transition() {
        with_tempdir_home(|| {
            let project_id = "p1".to_string();
            let task_id = "t-backlog-guard".to_string();

            let mut state = TaskState::new_backlog(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            orchestrator::write_task_state(&state).expect("write state");

            let args = vec![
                "--task".to_string(),
                task_id.clone(),
                "--project-id".to_string(),
                project_id.clone(),
                "--status".to_string(),
                "running".to_string(),
            ];
            let err = task_report(&args).expect_err("backlog->running must be rejected");
            match err {
                CliError::Usage(msg) => {
                    assert!(
                        msg.contains("task start"),
                        "message should point at the right verb, got: {msg}"
                    );
                }
                other => panic!("expected CliError::Usage, got {other:?}"),
            }

            // Status must not have been mutated.
            let after = orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after.status, TaskStatus::Backlog);
        });
    }

    #[test]
    fn pr_reopen_flips_changes_requested_to_open() {
        with_tempdir_home(|| {
            let project_id = "p1".to_string();
            let task_id = "t-reopen".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Review;
            orchestrator::write_task_state(&state).expect("write state");

            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");

            pr::update_pr(&project_id, &task_id, |p| {
                p.review_state = pr::ReviewState::ChangesRequested;
            })
            .expect("set changes_requested");
            orchestrator::update_task_state(&project_id, &task_id, |s| {
                s.status = TaskStatus::Running;
            })
            .expect("set running");

            let code = dispatch(&[
                "pr".into(),
                "reopen".into(),
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--comment".into(),
                "fixed it".into(),
            ]);
            assert_eq!(code, Some(0), "dispatch should succeed");

            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Open);
            let last = after_pr.comments.last().expect("comment appended");
            assert_eq!(last.author, "orchestrator");
            assert_eq!(last.body, "fixed it");

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Review);
            assert!(
                after_state
                    .note
                    .as_deref()
                    .unwrap_or("")
                    .contains("reopened for re-review"),
                "note should mention reopen, got {:?}",
                after_state.note
            );
        });
    }

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn git_run(root: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {} failed: stdout={} stderr={}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        git_run(root, &["init", "-q", "-b", "main"]);
        git_run(root, &["config", "user.email", "cc-hub-test@example.com"]);
        git_run(root, &["config", "user.name", "cc-hub-test"]);
        git_run(root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("seed.txt"), "seed\n").unwrap();
        git_run(root, &["add", "."]);
        git_run(root, &["commit", "-q", "-m", "seed"]);
        dir
    }

    fn seed_task_with_worktree(
        project_root: &std::path::Path,
        status: TaskStatus,
    ) -> (String, String, std::path::PathBuf) {
        let project_id = orchestrator::ensure_project_registered(project_root, "test-proj")
            .expect("register project");
        let task_id = "t-delete-test".to_string();
        let worktree_name = "wt".to_string();

        let wt_path = orchestrator::create_worktree(project_root, &task_id, &worktree_name, "main")
            .expect("create worktree");

        let mut state = TaskState::new(
            project_id.clone(),
            project_root.to_path_buf(),
            "do thing".into(),
        );
        state.task_id = task_id.clone();
        state.status = status;
        state.workers.push(orchestrator::Worker {
            agent_id: "claude".into(),
            agent_kind: cc_hub_lib::agent::AgentKind::Claude,
            tmux_name: "cc-hub-test-wkr".into(),
            cwd: wt_path.clone(),
            worktree: Some(worktree_name),
            readonly: false,
            spawned_at: 0,
        });
        orchestrator::write_task_state(&state).expect("write state");

        (project_id, task_id, wt_path)
    }

    #[test]
    fn task_show_json_emits_task_id() {
        with_tempdir_home(|| {
            let project_id = "p1".to_string();
            let task_id = "t-show".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "inspect me".into(),
            );
            state.task_id = task_id.clone();
            orchestrator::write_task_state(&state).expect("write state");

            let code = dispatch(&[
                "task".into(),
                "show".into(),
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--json".into(),
            ]);
            assert_eq!(code, Some(0), "dispatch should succeed");
        });
    }

    #[test]
    fn task_create_with_project_id_uses_registered_root() {
        with_tempdir_home(|| {
            let file = orchestrator::ProjectsFile {
                projects: vec![orchestrator::Project {
                    id: "p1".into(),
                    name: "p1".into(),
                    root: PathBuf::from("/tmp/p1"),
                    created_at: 0,
                    build_cmd: None,
                }],
            };
            orchestrator::save_projects(&file).expect("save");

            task_create(&[
                "--backlog".into(),
                "--project-id".into(),
                "p1".into(),
                "--prompt".into(),
                "foo".into(),
            ])
            .expect("task_create ok");

            let tasks_dir = orchestrator::project_state_dir("p1")
                .expect("dir")
                .join("tasks");
            let entries: Vec<_> = std::fs::read_dir(&tasks_dir)
                .expect("read tasks dir")
                .map(|e| e.expect("entry"))
                .collect();
            assert_eq!(entries.len(), 1, "expected exactly one task entry");
            let task_id = entries[0].file_name().to_string_lossy().into_owned();

            let state = orchestrator::read_task_state("p1", &task_id).expect("read state");
            assert_eq!(state.project_root, PathBuf::from("/tmp/p1"));
            assert_eq!(state.project_id, "p1");
            assert_eq!(state.status, TaskStatus::Backlog);

            let projects = orchestrator::load_projects();
            assert_eq!(projects.projects.len(), 1);
            assert_eq!(projects.projects[0].id, "p1");
        });
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
    fn task_list_plain_emits_one_row_per_task() {
        with_tempdir_home(|| {
            let project_id = "p-list".to_string();

            let mut a = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "first task".into(),
            );
            a.task_id = "t-aaaa".into();
            a.status = TaskStatus::Running;
            a.updated_at = 2_000;
            orchestrator::write_task_state(&a).expect("write a");

            let mut b = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "second task".into(),
            );
            b.task_id = "t-bbbb".into();
            b.status = TaskStatus::Backlog;
            b.updated_at = 1_000;
            orchestrator::write_task_state(&b).expect("write b");

            let args = vec!["--project-id".into(), project_id.clone()];
            task_list(&args).expect("task_list ok");
        });
    }

    #[test]
    fn task_show_function_succeeds_without_pr() {
        with_tempdir_home(|| {
            let project_id = "p1".to_string();
            let task_id = "t-show-fn".to_string();
            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "hello".into(),
            );
            state.task_id = task_id.clone();
            orchestrator::write_task_state(&state).expect("write state");
            let args = vec![
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--json".into(),
            ];
            task_show(&args).expect("task_show ok");
        });
    }

    #[test]
    fn task_list_status_filter_rejects_garbage() {
        let args = vec!["--status".into(), "nope".into()];
        match task_list(&args) {
            Err(CliError::Usage(_)) => {}
            other => panic!("expected Usage error, got {:?}", other),
        }
    }

    #[test]
    fn task_list_missing_tasks_dir_is_ok() {
        with_tempdir_home(|| {
            let args = vec!["--project-id".into(), "nonexistent".into()];
            task_list(&args).expect("missing tasks dir should be ok");
        });
    }

    #[test]
    fn task_list_dispatch_smoke() {
        let code = dispatch(&["task".into(), "list".into(), "--json".into()]);
        assert_eq!(code, Some(0));
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

    /// Regression: if `merge_lock::release` fails, `pr_finalize` must bail
    /// without flipping the task to `Done`. Otherwise a transient FS error
    /// strands a terminal-Done task holding the merge lock and blocks the
    /// project's merge queue until `STALE_TTL_SECS`.
    // Unix-only: the failure is simulated by chmod 0o555 to force EACCES on
    // lock removal — Windows has no equivalent permission semantics, and the
    // unix-only APIs below would break `cargo check --all-targets` there.
    #[cfg(unix)]
    #[test]
    fn pr_finalize_keeps_task_merging_when_release_fails() {
        use std::os::unix::fs::PermissionsExt;

        with_tempdir_home(|| {
            let project_id = "p1".to_string();
            let task_id = "t-finalize-fail".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Merging;
            orchestrator::write_task_state(&state).expect("write state");

            pr::create_pr(
                &state,
                "feature".into(),
                "main".into(),
                "title".into(),
                "desc".into(),
            )
            .expect("create pr");

            cc_hub_lib::merge_lock::acquire(&project_id, &task_id, None).expect("acquire lock");

            // Lock unlink requires write+exec on the parent dir; chmod 0o555
            // forces fs::remove_file to fail with EACCES, simulating the
            // transient FS error the bug fix guards against.
            let lock_path =
                cc_hub_lib::merge_lock::merge_lock_path(&project_id).expect("lock path");
            let parent = lock_path.parent().expect("parent dir").to_path_buf();
            let original_perms = std::fs::metadata(&parent).expect("metadata").permissions();
            let mut ro = original_perms.clone();
            ro.set_mode(0o555);
            std::fs::set_permissions(&parent, ro).expect("chmod ro");

            let result = pr_finalize(&[
                "--task".to_string(),
                task_id.clone(),
                "--project-id".to_string(),
                project_id.clone(),
                "--skip-build".to_string(),
            ]);

            // Restore perms before assertions so tempdir teardown works.
            std::fs::set_permissions(&parent, original_perms.clone()).expect("restore perms");

            assert!(
                result.is_err(),
                "pr_finalize should return Err when release fails, got {:?}",
                result
            );

            let after_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(
                after_state.status,
                TaskStatus::Merging,
                "task must remain Merging when release fails"
            );

            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(
                after_pr.review_state,
                pr::ReviewState::Open,
                "pr must remain Open when release fails (release runs before pr update)"
            );

            let holder = cc_hub_lib::merge_lock::current_holder(&project_id)
                .expect("read holder")
                .expect("lock present");
            assert_eq!(
                holder.task_id, task_id,
                "lock should still name this task — release never completed"
            );

            // Retry now that perms are restored: the second pr_finalize must
            // succeed and drive the task to Done.
            pr_finalize(&[
                "--task".to_string(),
                task_id.clone(),
                "--project-id".to_string(),
                project_id.clone(),
                "--skip-build".to_string(),
            ])
            .expect("retry pr_finalize after restore");

            let final_state =
                orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            assert_eq!(final_state.status, TaskStatus::Done);
            let final_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(final_pr.review_state, pr::ReviewState::Merged);
            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("read holder")
                    .is_none(),
                "lock must be released after successful retry"
            );
        });
    }

    #[test]
    fn task_delete_removes_state_and_worktree() {
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        with_tempdir_home(|| {
            let repo = init_repo();
            let (project_id, task_id, wt_path) =
                seed_task_with_worktree(repo.path(), TaskStatus::Backlog);

            assert!(wt_path.exists(), "worktree should exist before delete");
            let state_dir = orchestrator::task_state_dir(&project_id, &task_id).unwrap();
            assert!(state_dir.exists(), "state dir should exist before delete");

            let code = dispatch(&[
                "task".into(),
                "delete".into(),
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
            ]);
            assert_eq!(code, Some(0), "task delete should exit 0");
            assert!(!state_dir.exists(), "state dir should be gone after delete");
            assert!(
                !wt_path.exists(),
                "worktree dir should be gone after delete"
            );
        });
    }

    #[test]
    fn task_delete_refuses_running_without_force() {
        with_tempdir_home(|| {
            let project_id = "p-running".to_string();
            let task_id = "t-running".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/nonexistent"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Running;
            orchestrator::write_task_state(&state).expect("write state");

            let state_dir = orchestrator::task_state_dir(&project_id, &task_id).unwrap();
            assert!(state_dir.exists());

            let code = dispatch(&[
                "task".into(),
                "delete".into(),
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
            ]);
            assert_ne!(code, Some(0), "expected non-zero exit without --force");
            assert!(
                state_dir.exists(),
                "state dir must survive a refused delete"
            );
        });
    }

    #[test]
    fn task_delete_running_succeeds_with_force() {
        with_tempdir_home(|| {
            let project_id = "p-running-force".to_string();
            let task_id = "t-running-force".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/nonexistent"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Running;
            orchestrator::write_task_state(&state).expect("write state");

            let state_dir = orchestrator::task_state_dir(&project_id, &task_id).unwrap();
            assert!(state_dir.exists());

            let code = dispatch(&[
                "task".into(),
                "delete".into(),
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--force".into(),
            ]);
            assert_eq!(code, Some(0), "task delete --force should exit 0");
            assert!(
                !state_dir.exists(),
                "state dir should be gone after --force delete"
            );
        });
    }

    #[test]
    fn task_delete_refuses_merging_without_force() {
        with_tempdir_home(|| {
            let project_id = "p-merging".to_string();
            let task_id = "t-merging".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/nonexistent"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Merging;
            orchestrator::write_task_state(&state).expect("write state");

            let state_dir = orchestrator::task_state_dir(&project_id, &task_id).unwrap();

            let code = dispatch(&[
                "task".into(),
                "delete".into(),
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
            ]);
            assert_ne!(code, Some(0), "Merging must refuse delete without --force");
            assert!(
                state_dir.exists(),
                "state dir must survive a refused delete"
            );
        });
    }

    #[test]
    fn task_delete_merging_succeeds_with_force_and_releases_lock() {
        // A task wedged in Merging (e.g. its orchestrator died before
        // `pr finalize`) must be deletable under --force, and that delete
        // must release the project merge lock so the project isn't blocked
        // forever.
        with_tempdir_home(|| {
            let project_id = "p-merging-force".to_string();
            let task_id = "t-merging-force".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/nonexistent"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Merging;
            orchestrator::write_task_state(&state).expect("write state");

            cc_hub_lib::merge_lock::acquire(&project_id, &task_id, None).expect("acquire lock");
            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("current_holder")
                    .is_some(),
                "lock should be held before delete"
            );

            let state_dir = orchestrator::task_state_dir(&project_id, &task_id).unwrap();

            let code = dispatch(&[
                "task".into(),
                "delete".into(),
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
                "--force".into(),
            ]);
            assert_eq!(code, Some(0), "Merging delete --force should exit 0");
            assert!(
                !state_dir.exists(),
                "state dir should be gone after --force delete"
            );
            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("current_holder")
                    .is_none(),
                "merge lock must be released when a Merging task is deleted"
            );
        });
    }

    #[test]
    fn task_delete_releases_merge_lock() {
        with_tempdir_home(|| {
            let project_id = "p-lock-leak".to_string();
            let task_id = "t-lock-leak".to_string();

            let mut state = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/nonexistent"),
                "do thing".into(),
            );
            state.task_id = task_id.clone();
            state.status = TaskStatus::Backlog;
            orchestrator::write_task_state(&state).expect("write state");

            match cc_hub_lib::merge_lock::acquire(&project_id, &task_id, None)
                .expect("acquire lock")
            {
                cc_hub_lib::merge_lock::AcquireOutcome::Acquired => {}
                other => panic!("expected Acquired, got {:?}", other),
            }
            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("current_holder")
                    .is_some(),
                "lock should be held before delete"
            );

            let code = dispatch(&[
                "task".into(),
                "delete".into(),
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
            ]);
            assert_eq!(code, Some(0), "task delete should exit 0");
            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("current_holder")
                    .is_none(),
                "merge lock must be released by task delete"
            );
        });
    }

    #[test]
    fn pr_merge_refuses_dirty_worktree_without_demoting() {
        // A dirty feature-branch worktree must trip the step-1 preflight and
        // return a distinct `dirty_worktree` outcome — NOT proceed into the
        // merge (which would misclassify as a phantom conflict and demote the
        // approved PR). The PR stays Approved and the merge lock is released.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        with_tempdir_home(|| {
            let repo = init_repo();
            let (project_id, task_id, wt_path) =
                seed_task_with_worktree(repo.path(), TaskStatus::Review);

            // Approved PR on the worktree's branch.
            let state = orchestrator::read_task_state(&project_id, &task_id).expect("read state");
            let branch = orchestrator::worktree_branch(&task_id, "wt");
            pr::create_pr(&state, branch, "main".into(), "title".into(), "desc".into())
                .expect("create pr");
            pr::update_pr(&project_id, &task_id, |p| {
                p.review_state = pr::ReviewState::Approved;
            })
            .expect("approve pr");

            // Dirty the worktree: an uncommitted edit to a tracked file.
            std::fs::write(wt_path.join("seed.txt"), "dirtied in worktree\n")
                .expect("dirty worktree");

            let result = pr_merge(&[
                "--task".into(),
                task_id.clone(),
                "--project-id".into(),
                project_id.clone(),
            ]);
            match result {
                Err(CliError::Reported(msg)) => assert!(
                    msg.contains("worktree") && msg.contains("uncommitted"),
                    "expected dirty-worktree refusal, got: {}",
                    msg
                ),
                other => panic!("expected dirty-worktree refusal, got {:?}", other),
            }

            // PR must remain Approved (not demoted to Open).
            let after_pr = pr::read_pr(&project_id, &task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(
                after_pr.review_state,
                pr::ReviewState::Approved,
                "dirty-worktree refusal must not demote the PR"
            );

            // Merge lock released so the project isn't blocked.
            assert!(
                cc_hub_lib::merge_lock::current_holder(&project_id)
                    .expect("current_holder")
                    .is_none(),
                "merge lock must be released after a dirty-worktree refusal"
            );

            // The dirty edit is untouched — preflight must not mutate the tree.
            let body = std::fs::read_to_string(wt_path.join("seed.txt")).unwrap();
            assert_eq!(body, "dirtied in worktree\n");
        });
    }

    fn make_state_with_three_workers() -> TaskState {
        let mut state = TaskState::new(
            "p-wait".to_string(),
            PathBuf::from("/tmp/proj"),
            "do thing".into(),
        );
        state.task_id = "t-wait".to_string();
        state.workers.push(orchestrator::Worker {
            agent_id: "claude".into(),
            agent_kind: cc_hub_lib::agent::AgentKind::Claude,
            tmux_name: "cchub-1".into(),
            cwd: PathBuf::from("/tmp"),
            worktree: Some("fix".into()),
            readonly: false,
            spawned_at: 0,
        });
        state.workers.push(orchestrator::Worker {
            agent_id: "claude".into(),
            agent_kind: cc_hub_lib::agent::AgentKind::Claude,
            tmux_name: "cchub-2".into(),
            cwd: PathBuf::from("/tmp"),
            worktree: Some("docs".into()),
            readonly: false,
            spawned_at: 0,
        });
        state.workers.push(orchestrator::Worker {
            agent_id: "claude".into(),
            agent_kind: cc_hub_lib::agent::AgentKind::Claude,
            tmux_name: "cchub-ro".into(),
            cwd: PathBuf::from("/tmp"),
            worktree: None,
            readonly: true,
            spawned_at: 0,
        });
        state
    }

    #[test]
    fn resolve_wait_targets_resolves_worktree_to_tmux() {
        let state = make_state_with_three_workers();
        let targets = resolve_wait_targets(&state, &[], &["fix".to_string()], false).expect("ok");
        assert_eq!(targets, vec!["cchub-1".to_string()]);
    }

    #[test]
    fn resolve_wait_targets_unknown_worktree_errors() {
        let state = make_state_with_three_workers();
        let err = resolve_wait_targets(&state, &[], &["missing".to_string()], false)
            .expect_err("must error on unknown worktree");
        match err {
            CliError::Usage(msg) => {
                assert!(
                    msg.contains("missing"),
                    "error must mention the unknown worktree name: {}",
                    msg
                );
                assert!(
                    msg.contains("fix") && msg.contains("docs"),
                    "error must list known worktrees: {}",
                    msg
                );
            }
            other => panic!("expected CliError::Usage, got {:?}", other),
        }
    }

    #[test]
    fn resolve_wait_targets_union_of_flags_deduped() {
        let state = make_state_with_three_workers();
        let targets = resolve_wait_targets(
            &state,
            &["cchub-1".to_string()],
            &["fix".to_string(), "docs".to_string()],
            false,
        )
        .expect("ok");
        assert_eq!(
            targets,
            vec!["cchub-1".to_string(), "cchub-2".to_string()],
            "tmux first, then worktree-resolved tmux names, with dedup preserving first occurrence"
        );
    }

    #[test]
    fn resolve_wait_targets_empty_selection_errors() {
        let state = make_state_with_three_workers();
        let err = resolve_wait_targets(&state, &[], &[], false)
            .expect_err("must error when no selection flags given");
        assert!(matches!(err, CliError::Usage(_)));
    }

    /// Helper: seed a Done task with a Merged PR, returning (project_id, task_id).
    fn seed_merged_pr(project_id: &str, task_id: &str) {
        let mut state = TaskState::new(
            project_id.to_string(),
            PathBuf::from("/tmp/proj"),
            "do thing".into(),
        );
        state.task_id = task_id.to_string();
        state.status = TaskStatus::Done;
        orchestrator::write_task_state(&state).expect("write state");

        pr::create_pr(
            &state,
            "feature".into(),
            "main".into(),
            "title".into(),
            "desc".into(),
        )
        .expect("create pr");
        pr::update_pr(project_id, task_id, |p| {
            p.review_state = pr::ReviewState::Merged;
        })
        .expect("set merged");
    }

    #[test]
    fn pr_approve_refuses_merged_pr_and_leaves_task_unchanged() {
        with_tempdir_home(|| {
            let project_id = "p-approve-merged";
            let task_id = "t-approve-merged";
            seed_merged_pr(project_id, task_id);

            let argv = vec![
                "pr".to_string(),
                "approve".to_string(),
                "--task".to_string(),
                task_id.to_string(),
                "--project-id".to_string(),
                project_id.to_string(),
            ];
            // Nonzero exit via dispatch (exercises the JSON-error handler too).
            let code = dispatch(&argv);
            assert_eq!(code, Some(1), "approve on merged PR must exit nonzero");

            // PR stays Merged, task stays Done.
            let after_pr = pr::read_pr(project_id, task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Merged);
            let after_state =
                orchestrator::read_task_state(project_id, task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Done);

            // Direct call surfaces a Conflict error.
            let err = pr_approve(&argv[2..]).expect_err("must error");
            assert_eq!(err.kind(), "conflict");
        });
    }

    #[test]
    fn pr_request_changes_refuses_merged_pr_and_leaves_task_unchanged() {
        with_tempdir_home(|| {
            let project_id = "p-rc-merged";
            let task_id = "t-rc-merged";
            seed_merged_pr(project_id, task_id);

            let argv = vec![
                "pr".to_string(),
                "request-changes".to_string(),
                "--task".to_string(),
                task_id.to_string(),
                "--project-id".to_string(),
                project_id.to_string(),
                "--comment".to_string(),
                "please change".to_string(),
            ];
            let code = dispatch(&argv);
            assert_eq!(
                code,
                Some(1),
                "request-changes on merged PR must exit nonzero"
            );

            // Critically: the merged PR must NOT be resurrected, and the Done
            // task must NOT be forced back to Running.
            let after_pr = pr::read_pr(project_id, task_id)
                .expect("read pr")
                .expect("pr present");
            assert_eq!(after_pr.review_state, pr::ReviewState::Merged);
            assert!(
                after_pr.comments.is_empty(),
                "no comment should be appended on a refused request-changes"
            );
            let after_state =
                orchestrator::read_task_state(project_id, task_id).expect("read state");
            assert_eq!(after_state.status, TaskStatus::Done);

            let err = pr_request_changes(&argv[2..]).expect_err("must error");
            assert_eq!(err.kind(), "conflict");
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
