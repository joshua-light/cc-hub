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
    self, MergeOutcome, MergeRecord, TaskState, TaskStatus, Worker,
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
    wait_secs: Option<u64>,
    dry_run: bool,
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

    let record = MergeRecord {
        worktree: worktree_name.clone(),
        at: orchestrator::now_unix_secs(),
        outcome: outcome.clone(),
    };
    let _ = orchestrator::update_task_state(&project_id, &task_id, |s| {
        s.merges.push(record);
    });

    let payload = serde_json::json!({
        "ok": matches!(outcome, MergeOutcome::Ok),
        "worktree": worktree_name,
        "branch": branch,
        "main": main,
        "stdout": stdout,
        "stderr": stderr,
    });
    print_json(&payload);

    if matches!(outcome, MergeOutcome::Conflict { .. }) {
        return Err(CliError::Other(
            "merge produced conflicts; resolve in the worktree or main".into(),
        ));
    }
    Ok(())
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
        other => Err(CliError::Usage(format!(
            "unknown task verb: {} (try `report` or `create`)",
            other
        ))),
    }
}

fn task_report(args: &[String]) -> Result<(), CliError> {
    let f = parse_flags(args)?;
    let task_id = require_task(&f)?;
    let project_id = resolve_project_id(&f)?;

    let status = match f.status.as_deref() {
        None => None,
        Some("running") => Some(TaskStatus::Running),
        Some("done") => Some(TaskStatus::Done),
        Some("failed") => Some(TaskStatus::Failed),
        Some(other) => {
            return Err(CliError::Usage(format!(
                "--status must be running|done|failed (got {})",
                other
            )));
        }
    };

    let state = orchestrator::update_task_state(&project_id, &task_id, |s| {
        if let Some(st) = status {
            s.status = st;
        }
        if let Some(note) = f.note.clone() {
            s.note = Some(note);
        }
    })
    .map_err(|e| CliError::Other(format!("update state: {}", e)))?;

    print_json(&serde_json::json!({
        "ok": true,
        "task_id": state.task_id,
        "project_id": state.project_id,
        "status": state.status,
        "note": state.note,
        "updated_at": state.updated_at,
    }));
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

    let state = TaskState::new(project_id.clone(), cwd, prompt);
    orchestrator::write_task_state(&state)
        .map_err(|e| CliError::Other(format!("write state: {}", e)))?;

    print_json(&serde_json::json!({
        "ok": true,
        "task_id": state.task_id,
        "project_id": project_id,
    }));
    Ok(())
}

