//! CLI subcommands for the orchestrator layer.
//!
//! These run before the TUI starts up — when argv contains a known verb,
//! [`dispatch`] handles it and returns an exit code. The TUI in `main.rs`
//! never sees them.
//!
//! Argument parsing is hand-rolled to avoid a clap dep. Three verbs:
//!
//! - `cc-hub spawn-worker --task ID [--worktree NAME | --readonly] [--prompt P]`
//! - `cc-hub merge-worktree --task ID --worktree NAME`
//! - `cc-hub task report --task ID [--status S] [--note N]`
//!
//! All three derive `project-id` from the current working directory by
//! default; `--project-id ID` overrides for the rare case of operating
//! cross-project. They emit a single JSON line on stdout describing the
//! result so the orchestrator (a Claude Code session running under Bash)
//! can parse the outcome programmatically.
//!
//! Worktree mechanics live here too: `git -C <root> worktree add -b <branch>
//! <path> main`. cc-hub does **only** the mechanical git ops; deciding when
//! to spawn one and when to merge is the orchestrator's job.

use cc_hub_lib::orchestrator::{
    self, Artifact, MergeOutcome, MergeRecord, TaskState, TaskStatus, TodoItem, Worker,
};
use cc_hub_lib::scanner;
use cc_hub_lib::{models, send, spawn};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Cold claude sessions in fresh cwds (no JSONL history, no trust-store
/// entry) take longer to reach Idle than warm dev directories. 120s leaves
/// margin even for the slowest path; the timeout exists to surface
/// genuinely broken spawns, not to bound happy-path latency.
const DEFAULT_PROMPT_WAIT_SECS: u64 = 120;

pub fn dispatch(args: &[String]) -> Option<i32> {
    let (verb, rest) = args.split_first()?;
    match verb.as_str() {
        "spawn-worker" => Some(handle(spawn_worker(rest))),
        "merge-worktree" => Some(handle(merge_worktree(rest))),
        "task" => Some(handle(task_subcommand(rest))),
        "orchestrate" => Some(handle(orchestrate_subcommand(rest))),
        "pr" => Some(handle(pr_subcommand(rest))),
        _ => None,
    }
}

fn handle(result: Result<(), CliError>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(CliError::Usage(msg)) => {
            eprintln!("usage error: {}", msg);
            2
        }
        Err(CliError::Other(msg)) => {
            eprintln!("error: {}", msg);
            1
        }
    }
}

#[derive(Debug)]
enum CliError {
    Usage(String),
    Other(String),
}

impl From<String> for CliError {
    fn from(s: String) -> Self {
        CliError::Other(s)
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
    /// PR-flow flags. `--title` / `--description` for `pr create`,
    /// `--comment` + `--author` for `pr request-changes` / `pr comment`.
    title: Option<String>,
    description: Option<String>,
    comment: Option<String>,
    author: Option<String>,
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
                f.worktree = Some(next_value(args, &mut i, "--worktree")?);
            }
            "--readonly" => {
                f.readonly = true;
                i += 1;
            }
            "--prompt" => {
                f.prompt = Some(next_value(args, &mut i, "--prompt")?);
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
            other => {
                return Err(CliError::Usage(format!("unknown flag: {}", other)));
            }
        }
    }
    Ok(f)
}

fn next_value(args: &[String], i: &mut usize, name: &str) -> Result<String, CliError> {
    *i += 1;
    args.get(*i)
        .cloned()
        .map(|v| {
            *i += 1;
            v
        })
        .ok_or_else(|| CliError::Usage(format!("{} requires a value", name)))
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
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cwd: {}", e)))?;
    Ok(orchestrator::project_id_for_path(&cwd))
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

    if f.worktree.is_some() && f.readonly {
        return Err(CliError::Usage(
            "--worktree and --readonly are mutually exclusive".into(),
        ));
    }

    let state = orchestrator::read_task_state(&project_id, &task_id).map_err(|e| {
        CliError::Other(format!(
            "load state for {}/{}: {} (was the task created?)",
            project_id, task_id, e
        ))
    })?;
    let project_root = state.project_root.clone();

    let (cwd, worktree_name) = if let Some(name) = f.worktree.clone() {
        let main = orchestrator::detect_main_branch(&project_root);
        let path = orchestrator::create_worktree(&project_root, &task_id, &name, &main)
            .map_err(|e| CliError::Other(format!("create worktree: {}", e)))?;
        (path.to_string_lossy().into_owned(), Some(name))
    } else if f.readonly {
        (project_root.to_string_lossy().into_owned(), None)
    } else {
        return Err(CliError::Usage(
            "must pass either --worktree NAME or --readonly".into(),
        ));
    };

    let tmux_name = spawn::spawn_claude_session(&cwd, None)
        .map_err(|e| CliError::Other(format!("spawn session: {}", e)))?;

    let worker = Worker {
        tmux_name: tmux_name.clone(),
        cwd: PathBuf::from(&cwd),
        worktree: worktree_name.clone(),
        readonly: f.readonly,
        spawned_at: orchestrator::now_unix_secs(),
    };
    // Surface what the orchestrator dispatched in the project view, so a
    // glance at the Projects tab tells the user what each worker is doing.
    let prompt_preview = f.prompt.as_ref().map(|p| {
        let preview: String = p.chars().take(80).collect();
        format!("spawned worker: {}", preview)
    });
    orchestrator::update_task_state(&project_id, &task_id, move |s| {
        s.workers.push(worker);
        if let Some(note) = prompt_preview {
            s.note = Some(note);
        }
    })
    .map_err(|e| CliError::Other(format!("persist state: {}", e)))?;

    let mut prompt_status = "skipped";
    if let Some(prompt) = f.prompt.as_ref() {
        let wait = f.wait_secs.unwrap_or(DEFAULT_PROMPT_WAIT_SECS);
        match wait_until_idle_and_send(&tmux_name, prompt, Duration::from_secs(wait)) {
            Ok(()) => prompt_status = "sent",
            Err(e) => {
                // Don't fail the whole command — the session is up, the orch
                // can retry the dispatch. Surface the issue in the report.
                log::warn!("spawn-worker: prompt dispatch failed: {}", e);
                prompt_status = "deferred";
                eprintln!("warning: prompt dispatch failed ({}), session is up", e);
            }
        }
    }

    print_json(&serde_json::json!({
        "ok": true,
        "tmux": tmux_name,
        "cwd": cwd,
        "worktree": worktree_name,
        "readonly": f.readonly,
        "prompt_status": prompt_status,
        "task_id": task_id,
        "project_id": project_id,
    }));
    Ok(())
}

fn wait_until_idle_and_send(
    tmux_name: &str,
    prompt: &str,
    timeout: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    let deadline = started + timeout;
    loop {
        // Layered readiness, same shape as App::poll_pending_dispatch:
        //   1. scanner Idle + pane shows claude's empty `❯` input row.
        //      Tightest, preferred.
        //   2. scanner Idle + >=5s elapsed. Fallback for the case where
        //      claude renders something we don't recognise; without it,
        //      any cosmetic mismatch silently drops the prompt at the
        //      timeout boundary.
        let sessions = scanner::scan_sessions();
        let scanner_idle = sessions.iter().any(|s| {
            s.tmux_session.as_deref() == Some(tmux_name)
                && s.state == models::SessionState::Idle
        });
        if scanner_idle {
            let pane_ready = send::pane_ready_for_input(tmux_name);
            let aged_in = started.elapsed() >= Duration::from_secs(5);
            if pane_ready || aged_in {
                if !pane_ready {
                    log::info!(
                        "dispatch: pane_ready=false but {}s elapsed — sending anyway (target=[{}])",
                        started.elapsed().as_secs(),
                        tmux_name
                    );
                }
                return send::send_prompt(tmux_name, prompt)
                    .map_err(|e| format!("send_prompt: {}", e));
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{} did not become ready within {}s",
                tmux_name,
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
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

    let state = orchestrator::read_task_state(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("load state: {}", e)))?;
    let project_root = state.project_root.clone();
    let branch = orchestrator::worktree_branch(&task_id, &worktree_name);
    let main = orchestrator::detect_main_branch(&project_root);

    let (outcome, stdout, stderr) =
        orchestrator::merge_branch(&project_root, &main, &branch)
            .map_err(|e| CliError::Other(format!("merge: {}", e)))?;

    // Don't persist a MergeRecord for the dirty-tree pre-flight refusal —
    // the merge never started, so recording it as "attempted" would
    // mislead the Projects view. Conflict/Ok still get recorded.
    let is_preflight_block = matches!(outcome, MergeOutcome::BlockedByDirtyTree { .. });
    if !is_preflight_block {
        let record = MergeRecord {
            worktree: worktree_name.clone(),
            at: orchestrator::now_unix_secs(),
            outcome: outcome.clone(),
        };
        let _ = orchestrator::update_task_state(&project_id, &task_id, |s| {
            s.merges.push(record);
        });
    }

    let mut payload = serde_json::json!({
        "ok": matches!(outcome, MergeOutcome::Ok),
        "worktree": worktree_name,
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

    match outcome {
        MergeOutcome::Ok => Ok(()),
        MergeOutcome::Conflict { .. } => Err(CliError::Other(
            "merge produced conflicts; resolve in the worktree or main".into(),
        )),
        MergeOutcome::BlockedByDirtyTree { overlap } => Err(CliError::Other(format!(
            "merge blocked: working tree on `{}` has uncommitted edits in {} file(s) the branch also modified ({}); commit/stash/revert and retry",
            main,
            overlap.len(),
            overlap.join(", ")
        ))),
    }
}

// ─── orchestrate ─────────────────────────────────────────────────────────

fn orchestrate_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args.split_first().ok_or_else(|| {
        CliError::Usage("orchestrate <verb>: missing verb (try `start`)".into())
    })?;
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
/// Spawns `cc-hub-new` in the project root, waits up to `--wait-secs` (default
/// 60) for the new session to reach Idle, then dispatches the orchestrator
/// prompt as the first user message. Records the resulting tmux name in
/// state.json.
fn orchestrate_start(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let mut state = orchestrator::read_task_state(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("load state: {}", e)))?;

    let cc_hub_bin = std::env::current_exe()
        .map_err(|e| CliError::Other(format!("resolve cc-hub binary path: {}", e)))?;

    if f.dry_run {
        // Useful for verifying prompt content without paying for a session.
        let prompt = orchestrator::build_orchestrator_prompt(&state, &cc_hub_bin);
        println!("{}", prompt);
        return Ok(());
    }

    let cwd = state.project_root.to_string_lossy().into_owned();
    let tmux_name = spawn::spawn_claude_session(&cwd, None)
        .map_err(|e| CliError::Other(format!("spawn orchestrator: {}", e)))?;

    state.orchestrator_tmux = Some(tmux_name.clone());
    state.touch();
    orchestrator::write_task_state(&state)
        .map_err(|e| CliError::Other(format!("persist state: {}", e)))?;

    let prompt = orchestrator::build_orchestrator_prompt(&state, &cc_hub_bin);
    let wait = f.wait_secs.unwrap_or(DEFAULT_PROMPT_WAIT_SECS);

    let prompt_status = match wait_until_idle_and_send(
        &tmux_name,
        &prompt,
        Duration::from_secs(wait),
    ) {
        Ok(()) => "sent",
        Err(e) => {
            log::warn!("orchestrate start: dispatch failed: {}", e);
            eprintln!("warning: prompt dispatch failed ({}), session is up", e);
            "deferred"
        }
    };

    print_json(&serde_json::json!({
        "ok": true,
        "tmux": tmux_name,
        "cwd": cwd,
        "prompt_status": prompt_status,
        "task_id": task_id,
        "project_id": project_id,
    }));
    Ok(())
}

// ─── task ────────────────────────────────────────────────────────────────

fn task_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args
        .split_first()
        .ok_or_else(|| CliError::Usage("task <verb>: missing verb (try `report`)".into()))?;
    match verb.as_str() {
        "report" => task_report(rest),
        "create" => task_create(rest),
        "start" => task_start(rest),
        "artifact" => task_artifact_subcommand(rest),
        "todos" => task_todos_subcommand(rest),
        other => Err(CliError::Usage(format!(
            "unknown task verb: {} (try `report`, `create`, `start`, `artifact`, or `todos`)",
            other
        ))),
    }
}

/// `cc-hub task start --task ID [--project-id ID] [--wait-secs N]`
///
/// Flip a Backlog task to Running and spawn its orchestrator. Mirrors
/// `orchestrate start` but goes through `start_backlog_task` so it errors
/// cleanly if the task isn't in Backlog.
fn task_start(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let (_state, tmux_name, prompt) =
        orchestrator::start_backlog_task(&project_id, &task_id)
            .map_err(|e| CliError::Other(format!("start backlog task: {}", e)))?;

    let wait = f.wait_secs.unwrap_or(DEFAULT_PROMPT_WAIT_SECS);
    let prompt_status = match wait_until_idle_and_send(
        &tmux_name,
        &prompt,
        Duration::from_secs(wait),
    ) {
        Ok(()) => "sent",
        Err(e) => {
            log::warn!("task start: dispatch failed: {}", e);
            eprintln!("warning: prompt dispatch failed ({}), session is up", e);
            "deferred"
        }
    };

    print_json(&serde_json::json!({
        "ok": true,
        "tmux": tmux_name,
        "prompt_status": prompt_status,
        "task_id": task_id,
        "project_id": project_id,
    }));
    Ok(())
}

fn task_report(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let raw_status = match f.status.as_deref() {
        None => None,
        Some("running") => Some(TaskStatus::Running),
        Some("review") => Some(TaskStatus::Review),
        Some("merging") => Some(TaskStatus::Merging),
        Some("done") => Some(TaskStatus::Done),
        Some("failed") => Some(TaskStatus::Failed),
        Some("backlog") => Some(TaskStatus::Backlog),
        Some(other) => {
            return Err(CliError::Usage(format!(
                "--status must be running|review|merging|done|failed|backlog (got {})",
                other
            )));
        }
    };

    let prev_status = orchestrator::read_task_state(&project_id, &task_id)
        .ok()
        .map(|s| s.status);

    // Backlog is only a valid target from a Backlog state. Flipping a
    // running task to Backlog would hide it from the kanban while leaving
    // the orchestrator/tmux session alive — a zombie. Callers wanting to
    // abandon an active task should use `--status failed`, which runs the
    // standard cleanup path below.
    if raw_status.as_ref() == Some(&TaskStatus::Backlog)
        && prev_status.as_ref() != Some(&TaskStatus::Backlog)
    {
        return Err(CliError::Usage(
            "--status backlog is only valid from a Backlog state; \
             use --status failed to abandon an active task (cleanup runs automatically)"
                .into(),
        ));
    }

    // An orchestrator's `--status done` means "I'm finished" — it does NOT
    // mean the work is approved. Route that into Review so a human (or
    // future agentic reviewer) signs off via the TUI's `Space` keybind.
    // The exception: if the task is already in Review, an explicit `done`
    // is the approval path (used by `approve_review_task`'s subprocess
    // fallback, if any) — let it through. `failed` always lands as Failed.
    let effective_status = match (raw_status.clone(), prev_status.as_ref()) {
        (Some(TaskStatus::Done), prev) if prev != Some(&TaskStatus::Review) => {
            Some(TaskStatus::Review)
        }
        (other, _) => other,
    };

    let state = orchestrator::update_task_state(&project_id, &task_id, |s| {
        if let Some(st) = effective_status.clone() {
            s.status = st;
        }
        if let Some(note) = f.note.clone() {
            s.note = Some(note);
        }
        if let Some(summary) = f.summary.clone() {
            s.summary = Some(summary);
        }
    })
    .map_err(|e| CliError::Other(format!("update state: {}", e)))?;

    // Cleanup runs only when the task actually leaves the active flow:
    // - Failed (immediate)
    // - Done (only via Review → Done; fresh `done` reports go to Review and
    //   keep the orchestrator alive in case the human wants follow-up).
    let became_terminal = matches!(state.status, TaskStatus::Done | TaskStatus::Failed)
        && prev_status.as_ref() != Some(&state.status);
    if became_terminal {
        orchestrator::cleanup_task_sessions(&state);
    }

    print_json(&serde_json::json!({
        "ok": true,
        "task_id": state.task_id,
        "project_id": state.project_id,
        "status": state.status,
        "requested_status": raw_status,
        "note": state.note,
        "summary": state.summary,
        "updated_at": state.updated_at,
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

fn looks_like_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

fn task_artifact_add(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;
    let raw_path = f
        .path
        .clone()
        .ok_or_else(|| CliError::Usage("--path is required".into()))?;

    // Confirm the task exists before doing any filesystem work, so we don't
    // copy files into a directory that points at a nonexistent task.
    let _ = orchestrator::read_task_state(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("load state: {}", e)))?;

    let (kind, stored_path, basename_for_note) = if looks_like_url(&raw_path) {
        let kind = f.kind.clone().unwrap_or_else(|| "url".into());
        // Last URL segment is the closest thing to a "basename" for note text.
        let basename = raw_path
            .rsplit_once('/')
            .map(|(_, tail)| tail.to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| raw_path.clone());
        (kind, raw_path.clone(), basename)
    } else {
        let kind = f.kind.clone().unwrap_or_else(|| "file".into());
        let src = std::fs::canonicalize(&raw_path).map_err(|e| {
            CliError::Other(format!(
                "resolve source path {:?}: {} (does the file exist?)",
                raw_path, e
            ))
        })?;
        let meta = std::fs::metadata(&src)
            .map_err(|e| CliError::Other(format!("stat {}: {}", src.display(), e)))?;
        if meta.is_dir() {
            return Err(CliError::Other(format!(
                "{} is a directory; only single files are supported",
                src.display()
            )));
        }
        let basename = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| {
                CliError::Other(format!("{} has no file name", src.display()))
            })?;

        let dest_dir = orchestrator::task_state_dir(&project_id, &task_id)
            .ok_or_else(|| CliError::Other("no home dir".into()))?
            .join("artifacts");
        std::fs::create_dir_all(&dest_dir).map_err(|e| {
            CliError::Other(format!("create {}: {}", dest_dir.display(), e))
        })?;

        let ts = orchestrator::now_unix_secs();
        let dest = dest_dir.join(format!("{}-{}", ts, basename));
        std::fs::copy(&src, &dest).map_err(|e| {
            CliError::Other(format!(
                "copy {} -> {}: {}",
                src.display(),
                dest.display(),
                e
            ))
        })?;
        (kind, dest.to_string_lossy().into_owned(), basename)
    };

    let artifact = Artifact {
        kind: kind.clone(),
        path: stored_path.clone(),
        original: raw_path.clone(),
        caption: f.caption.clone(),
        added_at: orchestrator::now_unix_secs(),
    };
    let lead_note_suffix = if f.lead { " (lead)" } else { "" };
    let note = format!("artifact added: {} {}{}", kind, basename_for_note, lead_note_suffix);
    let mark_lead = f.lead;
    let state = orchestrator::update_task_state(&project_id, &task_id, |s| {
        s.artifacts.push(artifact.clone());
        if mark_lead {
            s.lead_artifact = Some(s.artifacts.len() - 1);
        }
        s.note = Some(note);
    })
    .map_err(|e| CliError::Other(format!("persist state: {}", e)))?;

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

    let state = orchestrator::read_task_state(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("load state: {}", e)))?;

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
    print_json(&serde_json::Value::Array(arr));
    Ok(())
}

// ─── task todos ──────────────────────────────────────────────────────────

fn task_todos_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args.split_first().ok_or_else(|| {
        CliError::Usage("task todos <verb>: missing verb (try `set`, `check`, `uncheck`, or `clear`)".into())
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
    let raw = f
        .items
        .clone()
        .ok_or_else(|| CliError::Usage("--items is required (newline-separated todo texts)".into()))?;

    let new_todos: Vec<TodoItem> = raw
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| TodoItem { text: l.to_string(), done: false })
        .collect();

    let state = orchestrator::update_task_state(&project_id, &task_id, |s| {
        s.todos = new_todos;
    })
    .map_err(|e| CliError::Other(format!("persist state: {}", e)))?;

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

    let pre = orchestrator::read_task_state(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("load state: {}", e)))?;
    if idx >= pre.todos.len() {
        return Err(CliError::Usage(format!(
            "--index {} out of range (have {} todo{})",
            idx,
            pre.todos.len(),
            if pre.todos.len() == 1 { "" } else { "s" }
        )));
    }

    let state = orchestrator::update_task_state(&project_id, &task_id, |s| {
        if let Some(t) = s.todos.get_mut(idx) {
            t.done = done;
        }
    })
    .map_err(|e| CliError::Other(format!("persist state: {}", e)))?;

    print_todos_result(&state);
    Ok(())
}

fn task_todos_clear(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let state = orchestrator::update_task_state(&project_id, &task_id, |s| {
        s.todos.clear();
    })
    .map_err(|e| CliError::Other(format!("persist state: {}", e)))?;

    print_todos_result(&state);
    Ok(())
}

/// `cc-hub task create --prompt "..." [--project-id ID] [--name NAME]`
///
/// Headless task creation — used by tests and tooling that wants to seed a
/// task without going through the TUI's `N → folder → prompt` flow.
fn task_create(args: &[String]) -> Result<(), CliError> {
    let mut f = Flags::default();
    let mut i = 0;
    let mut name: Option<String> = None;
    while i < args.len() {
        match args[i].as_str() {
            "--prompt" => f.prompt = Some(next_value(args, &mut i, "--prompt")?),
            "--project-id" => f.project_id = Some(next_value(args, &mut i, "--project-id")?),
            "--name" => name = Some(next_value(args, &mut i, "--name")?),
            "--backlog" => {
                f.backlog = true;
                i += 1;
            }
            other => return Err(CliError::Usage(format!("unknown flag: {}", other))),
        }
    }
    let prompt = f
        .prompt
        .clone()
        .ok_or_else(|| CliError::Usage("--prompt is required".into()))?;
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cwd: {}", e)))?;
    let project_id = f
        .project_id
        .clone()
        .unwrap_or_else(|| orchestrator::project_id_for_path(&cwd));
    let project_name = name.unwrap_or_else(|| {
        cwd.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| project_id.clone())
    });

    orchestrator::ensure_project_registered(&cwd, &project_name)
        .map_err(|e| CliError::Other(format!("register project: {}", e)))?;

    let state = if f.backlog {
        TaskState::new_backlog(project_id.clone(), cwd, prompt)
    } else {
        TaskState::new(project_id.clone(), cwd, prompt)
    };
    orchestrator::write_task_state(&state)
        .map_err(|e| CliError::Other(format!("write state: {}", e)))?;

    print_json(&serde_json::json!({
        "ok": true,
        "task_id": state.task_id,
        "project_id": project_id,
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
//   pr approve  → PR: Open|ChangesRequested → Approved (task stays Review)
//   pr merge    → acquires merge.lock, merges main → branch then branch →
//                 main; lock stays held; task: Review → Merging
//   pr finalize → releases merge.lock; task: Merging → Done; PR: Merged
//
// `pr show` is a read-only convenience for inspection / scripting.

fn pr_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args.split_first().ok_or_else(|| {
        CliError::Usage(
            "pr <verb>: missing verb (try `create`, `show`, `approve`, `request-changes`, `comment`, `merge`, `finalize`)".into(),
        )
    })?;
    match verb.as_str() {
        "create" => pr_create(rest),
        "show" => pr_show(rest),
        "approve" => pr_approve(rest),
        "request-changes" => pr_request_changes(rest),
        "comment" => pr_comment(rest),
        "merge" => pr_merge(rest),
        "finalize" => pr_finalize(rest),
        other => Err(CliError::Usage(format!(
            "unknown pr verb: {} (try `create`, `show`, `approve`, `request-changes`, `comment`, `merge`, `finalize`)",
            other
        ))),
    }
}

fn pr_to_json(pr: &cc_hub_lib::pr::PullRequest) -> serde_json::Value {
    serde_json::json!({
        "id": pr.id,
        "task_id": pr.task_id,
        "project_id": pr.project_id,
        "branch": pr.branch,
        "base": pr.base,
        "title": pr.title,
        "description": pr.description,
        "review_state": match pr.review_state {
            cc_hub_lib::pr::ReviewState::Open => "open",
            cc_hub_lib::pr::ReviewState::ChangesRequested => "changes_requested",
            cc_hub_lib::pr::ReviewState::Approved => "approved",
            cc_hub_lib::pr::ReviewState::Merged => "merged",
            cc_hub_lib::pr::ReviewState::Closed => "closed",
        },
        "comments": pr.comments,
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

    let state = orchestrator::read_task_state(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("load state: {}", e)))?;

    if cc_hub_lib::pr::read_pr(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("read pr: {}", e)))?
        .is_some()
    {
        return Err(CliError::Other(
            "a PR already exists for this task; use `pr show` to inspect".into(),
        ));
    }

    let branch = orchestrator::worktree_branch(&task_id, &worktree_name);
    let base = orchestrator::detect_main_branch(&state.project_root);

    let pr = cc_hub_lib::pr::create_pr(&state, branch, base, title, description)
        .map_err(|e| CliError::Other(format!("create pr: {}", e)))?;

    // PR open → task transitions Running → Review. The orchestrator's
    // tmux stays alive so it can iterate when the user requests changes.
    orchestrator::update_task_state(&project_id, &task_id, |s| {
        s.status = TaskStatus::Review;
        s.note = Some(format!("PR #{}: {}", pr.id, pr.title));
    })
    .map_err(|e| CliError::Other(format!("update state: {}", e)))?;

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
        .ok_or_else(|| CliError::Other("no PR for this task".into()))?;
    print_json(&serde_json::json!({
        "ok": true,
        "pr": pr_to_json(&pr),
    }));
    Ok(())
}

fn pr_approve(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let state = orchestrator::read_task_state(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("load state: {}", e)))?;
    let project_root = state.project_root.clone();

    let pr_branch = cc_hub_lib::pr::read_pr(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("read pr: {}", e)))?
        .ok_or_else(|| CliError::Other("no PR for this task".into()))?
        .branch
        .clone();
    let base_branch = orchestrator::detect_main_branch(&project_root);

    // Snapshot SHAs at approval — used by `pr merge` to detect whether
    // main moved before the merge fired (auto-approve heuristic).
    let branch_sha = git_rev_parse(&project_root, &pr_branch).ok();
    let base_sha = git_rev_parse(&project_root, &base_branch).ok();

    let pr = cc_hub_lib::pr::update_pr(&project_id, &task_id, |p| {
        p.review_state = cc_hub_lib::pr::ReviewState::Approved;
        p.approved_at_branch_sha = branch_sha;
        p.approved_at_base_sha = base_sha;
    })
    .map_err(|e| CliError::Other(format!("update pr: {}", e)))?;

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

    let now = orchestrator::now_unix_secs();
    let pr = cc_hub_lib::pr::update_pr(&project_id, &task_id, |p| {
        p.review_state = cc_hub_lib::pr::ReviewState::ChangesRequested;
        p.comments.push(cc_hub_lib::pr::Comment {
            author: author.clone(),
            at: now,
            body: comment.clone(),
        });
    })
    .map_err(|e| CliError::Other(format!("update pr: {}", e)))?;

    // Changes requested → task goes back to Running so the orchestrator
    // can iterate. Its tmux is still alive (Review keeps it alive).
    orchestrator::update_task_state(&project_id, &task_id, |s| {
        s.status = TaskStatus::Running;
        s.note = Some(format!("PR #{}: changes requested", pr.id));
    })
    .map_err(|e| CliError::Other(format!("update state: {}", e)))?;

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

    let now = orchestrator::now_unix_secs();
    let pr = cc_hub_lib::pr::update_pr(&project_id, &task_id, |p| {
        p.comments.push(cc_hub_lib::pr::Comment {
            author: author.clone(),
            at: now,
            body: body.clone(),
        });
    })
    .map_err(|e| CliError::Other(format!("update pr: {}", e)))?;

    print_json(&serde_json::json!({
        "ok": true,
        "pr": pr_to_json(&pr),
    }));
    Ok(())
}

fn pr_merge(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let state = orchestrator::read_task_state(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("load state: {}", e)))?;
    let project_root = state.project_root.clone();
    let pr = cc_hub_lib::pr::read_pr(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("read pr: {}", e)))?
        .ok_or_else(|| CliError::Other("no PR for this task".into()))?;

    if pr.review_state != cc_hub_lib::pr::ReviewState::Approved {
        return Err(CliError::Other(format!(
            "PR is not approved (state: {:?}); approve it first via the TUI or `cc-hub pr approve`",
            pr.review_state
        )));
    }

    // Acquire the project-wide merge lock. Held across the entire merging
    // phase — released by `pr finalize` after /simplify and /bump.
    let acquire = cc_hub_lib::merge_lock::acquire(
        &project_id,
        &task_id,
        state.orchestrator_tmux.as_deref(),
    )
    .map_err(|e| CliError::Other(format!("acquire merge lock: {}", e)))?;
    if let cc_hub_lib::merge_lock::AcquireOutcome::Held(holder) = acquire {
        print_json(&serde_json::json!({
            "ok": false,
            "locked": true,
            "holder_task": holder.task_id,
            "since": holder.acquired_at,
            "recipe": "Another task currently holds the merge lock. Poll until clear (re-run `cc-hub pr merge`) or wait for it to release.",
        }));
        return Err(CliError::Other(format!(
            "merge lock held by task {}",
            holder.task_id
        )));
    }

    // Step 1: bring main into the feature branch so the conflict
    // resolution happens on the feature branch (where the worker can
    // re-resolve cleanly), not on main itself.
    let worktree_path = match resolve_worktree_path(&state, &pr.branch) {
        Some(p) => p,
        None => {
            return Err(CliError::Other(format!(
                "could not resolve worktree path for branch {} \
                 (no Worker record matches; was the worktree removed?)",
                pr.branch
            )));
        }
    };

    let merge_into_feature = orchestrator::run_git(
        &worktree_path,
        &[
            "merge",
            "--no-ff",
            "-m",
            &format!("cc-hub: merge {} into {}", pr.base, pr.branch),
            &pr.base,
        ],
    )
    .map_err(|e| CliError::Other(format!("git merge {} into branch: {}", pr.base, e)))?;

    if !merge_into_feature.status_ok {
        // Conflicts merging main into the feature branch. By the
        // PR-flow design's auto-approve rule (only *clean* resolutions
        // skip re-review), conflicts demote the PR back to Open: the
        // user must re-approve once the orchestrator commits the
        // resolution, since the diff they previously approved no
        // longer matches what would land. The merge lock is released
        // so other tasks can proceed in the meantime.
        let conflicting = git_conflicting_paths(&worktree_path).unwrap_or_default();

        // Abort the in-progress merge so the worktree returns to a
        // clean state — the orchestrator will re-merge main once the
        // user re-approves. We don't surface abort failures: if abort
        // itself fails, the orchestrator can recover manually.
        let _ = orchestrator::run_git(&worktree_path, &["merge", "--abort"]);

        let comment_body = format!(
            "Auto-demoted to Open: merging `{}` into the feature branch produced conflicts in {} \
             file(s) ({}). cc-hub's auto-approve rule only accepts clean resolutions; resolve in \
             the worktree, push the resolution commit, then ask the reviewer to re-approve.",
            pr.base,
            conflicting.len(),
            conflicting.join(", "),
        );
        let now = orchestrator::now_unix_secs();
        let _ = cc_hub_lib::pr::update_pr(&project_id, &task_id, |p| {
            p.review_state = cc_hub_lib::pr::ReviewState::Open;
            p.approved_at_branch_sha = None;
            p.approved_at_base_sha = None;
            p.comments.push(cc_hub_lib::pr::Comment {
                author: "cc-hub".into(),
                at: now,
                body: comment_body.clone(),
            });
        });
        let _ = orchestrator::update_task_state(&project_id, &task_id, |s| {
            s.status = TaskStatus::Review;
            s.note = Some(format!(
                "PR #{}: conflicts during merge — re-review required",
                pr.id
            ));
        });
        let _ = cc_hub_lib::merge_lock::release(&project_id, &task_id);

        print_json(&serde_json::json!({
            "ok": false,
            "phase": "merge_main_into_branch",
            "demoted_to": "open",
            "conflicting_paths": conflicting,
            "stdout": merge_into_feature.stdout,
            "stderr": merge_into_feature.stderr,
            "recipe": "Resolve conflicts in the worktree, commit the resolution, then ask the reviewer to re-approve before re-running `cc-hub pr merge`. The merge lock has been released.",
        }));
        return Err(CliError::Other(
            "conflict merging main into the feature branch — PR demoted to Open".into(),
        ));
    }

    // Step 2: dirty-tree preflight on main. Distinct from cross-task
    // conflicts (which the merge lock already handles) — this catches
    // the user's local uncommitted edits.
    let changed = orchestrator::branch_changed_paths(&project_root, &pr.base, &pr.branch)
        .map_err(|e| CliError::Other(format!("diff branch: {}", e)))?;
    let dirty: std::collections::BTreeSet<String> =
        orchestrator::dirty_paths(&project_root)
            .map_err(|e| CliError::Other(format!("git status: {}", e)))?
            .into_iter()
            .collect();
    let branch_files: std::collections::BTreeSet<String> = changed.iter().cloned().collect();
    let overlap: Vec<String> = dirty.intersection(&branch_files).cloned().collect();
    if !overlap.is_empty() {
        // Release the lock so other tasks can merge while the user
        // cleans up their working tree. The PR remains Approved; the
        // orchestrator simply re-runs `pr merge` once the user has
        // committed/stashed/reverted.
        let _ = cc_hub_lib::merge_lock::release(&project_id, &task_id);
        print_json(&serde_json::json!({
            "ok": false,
            "phase": "preflight",
            "blocked_by_dirty_tree": true,
            "overlap": overlap,
            "recipe": "Commit, stash, or revert the listed paths on the target branch, then re-run `cc-hub pr merge`. The merge lock has been released.",
        }));
        return Err(CliError::Other(
            "merge blocked: working tree on target branch has overlapping uncommitted edits"
                .into(),
        ));
    }

    // Step 3: merge feature branch into main. Should be conflict-free
    // since we already merged main into the branch in step 1.
    let checkout = orchestrator::run_git(&project_root, &["checkout", &pr.base])
        .map_err(|e| CliError::Other(format!("git checkout: {}", e)))?;
    if !checkout.status_ok {
        return Err(CliError::Other(format!(
            "git checkout {} failed: {}",
            pr.base,
            checkout.stderr.trim()
        )));
    }
    let msg = format!("cc-hub: merge {} into {} (PR #{})", pr.branch, pr.base, pr.id);
    let merge_into_main = orchestrator::run_git(
        &project_root,
        &["merge", "--no-ff", "-m", &msg, &pr.branch],
    )
    .map_err(|e| CliError::Other(format!("git merge: {}", e)))?;

    if !merge_into_main.status_ok {
        // Should be rare given step 1, but possible if main moved
        // concurrently inside the lock window (it shouldn't, since
        // the lock serialises merges). Abort to leave main clean,
        // release the lock, and surface to the orchestrator.
        let conflicting = git_conflicting_paths(&project_root).unwrap_or_default();
        let _ = orchestrator::run_git(&project_root, &["merge", "--abort"]);
        let _ = cc_hub_lib::merge_lock::release(&project_id, &task_id);
        print_json(&serde_json::json!({
            "ok": false,
            "phase": "merge_branch_into_main",
            "conflicting_paths": conflicting,
            "stdout": merge_into_main.stdout,
            "stderr": merge_into_main.stderr,
            "recipe": "Unexpected conflict merging into main (the merge lock should have prevented this — investigate before retrying).",
        }));
        return Err(CliError::Other("conflict merging into main".into()));
    }

    // Transition task to Merging. /simplify and /bump still need to run;
    // `pr finalize` flips to Done afterwards.
    orchestrator::update_task_state(&project_id, &task_id, |s| {
        s.status = TaskStatus::Merging;
        s.note = Some(format!("PR #{}: merged; running /simplify + /bump", pr.id));
        s.merges.push(MergeRecord {
            worktree: pr
                .branch
                .strip_prefix(&format!("cc-hub/{}-", task_id))
                .unwrap_or(&pr.branch)
                .to_string(),
            at: orchestrator::now_unix_secs(),
            outcome: MergeOutcome::Ok,
        });
    })
    .map_err(|e| CliError::Other(format!("update state: {}", e)))?;

    print_json(&serde_json::json!({
        "ok": true,
        "phase": "merged",
        "branch": pr.branch,
        "base": pr.base,
        "stdout": merge_into_main.stdout,
        "next": "Run /simplify, then /bump, then `cc-hub pr finalize --task <id>` to release the merge lock and mark the task done.",
    }));
    Ok(())
}

fn pr_finalize(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    cc_hub_lib::pr::update_pr(&project_id, &task_id, |p| {
        p.review_state = cc_hub_lib::pr::ReviewState::Merged;
    })
    .map_err(|e| CliError::Other(format!("update pr: {}", e)))?;

    let state = orchestrator::update_task_state(&project_id, &task_id, |s| {
        s.status = TaskStatus::Done;
    })
    .map_err(|e| CliError::Other(format!("update state: {}", e)))?;

    let released = cc_hub_lib::merge_lock::release(&project_id, &task_id)
        .map_err(|e| CliError::Other(format!("release merge lock: {}", e)))?;

    // Cleanup the orchestrator tmux now that the task is fully done.
    orchestrator::cleanup_task_sessions(&state);

    print_json(&serde_json::json!({
        "ok": true,
        "released": released,
        "task_id": task_id,
        "status": "done",
    }));
    Ok(())
}

fn git_rev_parse(root: &std::path::Path, rev: &str) -> Result<String, String> {
    let out = orchestrator::run_git(root, &["rev-parse", rev])
        .map_err(|e| format!("git rev-parse: {}", e))?;
    if !out.status_ok {
        return Err(format!("git rev-parse {} failed: {}", rev, out.stderr.trim()));
    }
    Ok(out.stdout.trim().to_string())
}

/// `git diff --name-only --diff-filter=U` lists files with unresolved
/// conflicts. Repo-relative paths.
fn git_conflicting_paths(root: &std::path::Path) -> Result<Vec<String>, String> {
    let out = orchestrator::run_git(
        root,
        &["diff", "--name-only", "--diff-filter=U", "-z"],
    )
    .map_err(|e| format!("git diff (conflicts): {}", e))?;
    if !out.status_ok {
        return Err(format!(
            "git diff failed: {}",
            out.stderr.trim()
        ));
    }
    Ok(out
        .stdout
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

/// Locate the worktree directory for `branch` by checking the task's
/// recorded workers. Falls back to the conventional `<root>/.cc-hub-wt/`
/// path layout if no Worker record matches.
fn resolve_worktree_path(
    state: &TaskState,
    branch: &str,
) -> Option<PathBuf> {
    for w in &state.workers {
        if let Some(name) = &w.worktree {
            let expected_branch = orchestrator::worktree_branch(&state.task_id, name);
            if expected_branch == branch {
                return Some(w.cwd.clone());
            }
        }
    }
    // Fallback: parse the branch name (cc-hub/<task>-<name>) and rebuild.
    let stripped = branch.strip_prefix("cc-hub/")?;
    let prefix = format!("{}-", state.task_id);
    let name = stripped.strip_prefix(&prefix)?;
    Some(orchestrator::worktree_path(&state.project_root, &state.task_id, name))
}

