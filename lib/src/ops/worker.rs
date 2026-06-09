//! Worker spawn / merge / wait ops.
//!
//! Bodies of `spawn-worker`, `merge-worktree`, and `worker wait`, plus the
//! prompt-dispatch readiness helper (`wait_until_idle_and_send`) and the
//! `worker wait` target resolver (`resolve_wait_targets`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::models;
use crate::ops::OpError;
use crate::orchestrator::{self, MergeOutcome, MergeRecord, TaskState, Worker};
use crate::{config, scanner, send, spawn};

/// Cold claude sessions in fresh cwds (no JSONL history, no trust-store
/// entry) take longer to reach Idle than warm dev directories. 120s leaves
/// margin even for the slowest path; the timeout exists to surface
/// genuinely broken spawns, not to bound happy-path latency.
pub const DEFAULT_PROMPT_WAIT_SECS: u64 = 120;

fn find_by_tmux<'a>(
    sessions: &'a [models::SessionInfo],
    tmux: &str,
) -> Option<&'a models::SessionInfo> {
    sessions
        .iter()
        .find(|s| s.tmux_session.as_deref() == Some(tmux))
}

/// Block until `tmux_name` reaches a prompt-ready state, then send `prompt`.
///
/// Layered readiness, same shape as App::poll_pending_dispatch:
///   1. scanner Idle + pane shows claude's empty `❯` input row. Tightest,
///      preferred.
///   2. scanner Idle + >=5s elapsed. Fallback for the case where claude
///      renders something we don't recognise; without it, any cosmetic
///      mismatch silently drops the prompt at the timeout boundary.
pub fn wait_until_idle_and_send(
    tmux_name: &str,
    prompt: &str,
    timeout: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    let deadline = started + timeout;
    loop {
        let sessions = scanner::scan_sessions();
        let scanner_idle = find_by_tmux(&sessions, tmux_name)
            .is_some_and(|s| s.state == models::SessionState::Idle);
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

/// Outcome of dispatching a prompt to a freshly spawned session.
pub enum PromptStatus {
    /// The prompt was sent (or the agent accepted it as an initial prompt).
    Sent,
    /// No prompt was supplied — nothing to dispatch.
    Skipped,
    /// Dispatch failed; the session is up but the prompt could not be
    /// delivered. Carries the human warning string the caller should
    /// surface on stderr.
    Deferred(String),
}

impl PromptStatus {
    /// The stable string used in the `prompt_status` JSON field.
    pub fn as_str(&self) -> &'static str {
        match self {
            PromptStatus::Sent => "sent",
            PromptStatus::Skipped => "skipped",
            PromptStatus::Deferred(_) => "deferred",
        }
    }
}

/// Options for [`spawn_worker`].
pub struct SpawnWorkerOpts {
    pub worktree: Option<String>,
    pub readonly: bool,
    pub prompt: Option<String>,
    pub agent: Option<String>,
    pub wait_secs: Option<u64>,
}

/// Result of [`spawn_worker`].
pub struct SpawnWorkerOutcome {
    pub agent_id: String,
    pub agent_kind: crate::agent::AgentKind,
    pub tmux: String,
    pub cwd: String,
    pub worktree: Option<String>,
    pub readonly: bool,
    pub prompt_status: PromptStatus,
}

/// `cc-hub spawn-worker` body: create a worktree (or attach readonly), spawn
/// the worker session, persist the `Worker` record, and dispatch the initial
/// prompt.
pub fn spawn_worker(
    project_id: &str,
    task_id: &str,
    opts: SpawnWorkerOpts,
) -> Result<SpawnWorkerOutcome, OpError> {
    if opts.worktree.is_some() && opts.readonly {
        return Err(OpError::Usage(
            "--worktree and --readonly are mutually exclusive".into(),
        ));
    }

    let state = orchestrator::read_task_state(project_id, task_id).map_err(|e| {
        OpError::Other(format!(
            "load state for {}/{}: {} (was the task created?)",
            project_id, task_id, e
        ))
    })?;
    let project_root = state.project_root.clone();

    let (cwd, worktree_name) = if let Some(name) = opts.worktree.clone() {
        let main = orchestrator::detect_main_branch(&project_root);
        let path = orchestrator::create_worktree(&project_root, task_id, &name, &main)
            .map_err(|e| OpError::Other(format!("create worktree: {}", e)))?;
        (path.to_string_lossy().into_owned(), Some(name))
    } else if opts.readonly {
        (project_root.to_string_lossy().into_owned(), None)
    } else {
        return Err(OpError::Usage(
            "must pass either --worktree NAME or --readonly".into(),
        ));
    };

    let agent_id = opts
        .agent
        .clone()
        .unwrap_or_else(|| state.orchestrator_agent_id.clone());
    let agent = config::get()
        .agent(&agent_id)
        .ok_or_else(|| OpError::Other(format!("unknown worker agent: {}", agent_id)))?;
    let initial_prompt = if agent.supports_initial_prompt() {
        opts.prompt.as_deref()
    } else {
        None
    };
    let tmux_name =
        spawn::spawn_agent_session(&agent_id, &cwd, None, initial_prompt, opts.readonly)
            .map_err(|e| OpError::Other(format!("spawn session: {}", e)))?;

    let worker = Worker {
        agent_id: agent_id.clone(),
        agent_kind: agent.kind,
        tmux_name: tmux_name.clone(),
        cwd: PathBuf::from(&cwd),
        worktree: worktree_name.clone(),
        readonly: opts.readonly,
        spawned_at: orchestrator::now_unix_secs(),
    };
    orchestrator::update_task_state(project_id, task_id, move |s| {
        s.workers.push(worker);
    })
    .map_err(|e| OpError::Other(format!("persist state: {}", e)))?;

    let mut prompt_status = PromptStatus::Skipped;
    if agent.supports_initial_prompt() {
        if opts.prompt.is_some() {
            prompt_status = PromptStatus::Sent;
        }
    } else if let Some(prompt) = opts.prompt.as_ref() {
        let wait = opts.wait_secs.unwrap_or(DEFAULT_PROMPT_WAIT_SECS);
        match wait_until_idle_and_send(&tmux_name, prompt, Duration::from_secs(wait)) {
            Ok(()) => prompt_status = PromptStatus::Sent,
            Err(e) => {
                log::warn!("spawn-worker: prompt dispatch failed: {}", e);
                prompt_status = PromptStatus::Deferred(format!(
                    "prompt dispatch failed ({}), session is up",
                    e
                ));
            }
        }
    }

    Ok(SpawnWorkerOutcome {
        agent_id,
        agent_kind: agent.kind,
        tmux: tmux_name,
        cwd,
        worktree: worktree_name,
        readonly: opts.readonly,
        prompt_status,
    })
}

/// Result of [`merge_worktree`].
pub struct MergeWorktreeOutcome {
    pub outcome: MergeOutcome,
    pub worktree: String,
    pub branch: String,
    pub main: String,
    pub stdout: String,
    pub stderr: String,
}

/// `cc-hub merge-worktree` body: merge the worker branch into main and record
/// a `MergeRecord` (except for the dirty-tree pre-flight refusal, which never
/// started a merge).
pub fn merge_worktree(
    project_id: &str,
    task_id: &str,
    worktree_name: &str,
) -> Result<MergeWorktreeOutcome, OpError> {
    let state = orchestrator::read_task_state(project_id, task_id)
        .map_err(|e| OpError::Other(format!("load state: {}", e)))?;
    let project_root = state.project_root.clone();
    let branch = orchestrator::worktree_branch(task_id, worktree_name);
    let main = orchestrator::detect_main_branch(&project_root);

    let (outcome, stdout, stderr) = orchestrator::merge_branch(&project_root, &main, &branch)
        .map_err(|e| OpError::Other(format!("merge: {}", e)))?;

    // Don't persist a MergeRecord for the dirty-tree pre-flight refusal —
    // the merge never started, so recording it as "attempted" would
    // mislead the Projects view. Conflict/Ok still get recorded.
    let is_preflight_block = matches!(outcome, MergeOutcome::BlockedByDirtyTree { .. });
    if !is_preflight_block {
        let record = MergeRecord {
            worktree: worktree_name.to_string(),
            at: orchestrator::now_unix_secs(),
            outcome: outcome.clone(),
        };
        let _ = orchestrator::update_task_state(project_id, task_id, |s| {
            s.merges.push(record);
        });
    }

    Ok(MergeWorktreeOutcome {
        outcome,
        worktree: worktree_name.to_string(),
        branch,
        main,
        stdout,
        stderr,
    })
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
pub fn resolve_wait_targets(
    state: &TaskState,
    tmux_targets: &[String],
    worktree_targets: &[String],
    all: bool,
) -> Result<Vec<String>, OpError> {
    if tmux_targets.is_empty() && worktree_targets.is_empty() && !all {
        return Err(OpError::Usage(
            "must pass --tmux NAME ..., --worktree NAME ..., or --all".into(),
        ));
    }

    let known_tmux: std::collections::HashSet<&str> =
        state.workers.iter().map(|w| w.tmux_name.as_str()).collect();
    for t in tmux_targets {
        if !known_tmux.contains(t.as_str()) {
            return Err(OpError::Usage(format!(
                "--tmux {}: not a worker of task {} (known: [{}])",
                t,
                state.task_id,
                state
                    .workers
                    .iter()
                    .map(|w| w.tmux_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }

    let mut targets: Vec<String> = Vec::new();
    for t in tmux_targets {
        targets.push(t.clone());
    }
    for wt in worktree_targets {
        match state
            .workers
            .iter()
            .find(|w| w.worktree.as_deref() == Some(wt.as_str()))
        {
            Some(w) => targets.push(w.tmux_name.clone()),
            None => {
                let known_wt: Vec<&str> = state
                    .workers
                    .iter()
                    .filter_map(|w| w.worktree.as_deref())
                    .collect();
                return Err(OpError::Usage(format!(
                    "--worktree {}: not a worker of task {} (known worktrees: [{}])",
                    wt,
                    state.task_id,
                    known_wt.join(", ")
                )));
            }
        }
    }
    if all {
        for w in &state.workers {
            targets.push(w.tmux_name.clone());
        }
    }

    let mut seen = std::collections::HashSet::new();
    targets.retain(|t| seen.insert(t.clone()));
    Ok(targets)
}

/// A progress snapshot emitted during [`worker_wait`] while `--progress` is on.
pub struct WaitProgress {
    pub elapsed_secs: u64,
    pub pending: Vec<String>,
    pub done: Vec<String>,
}

/// Final result of [`worker_wait`].
pub struct WorkerWaitOutcome {
    pub all_done: bool,
    pub timed_out: bool,
    pub elapsed_secs: u64,
    /// Per-target JSON values, in selection order — built here because the
    /// per-worker shape (tmux/state/done/last_user_message) is domain data
    /// the scanner produces.
    pub workers: Vec<serde_json::Value>,
}

/// `cc-hub worker wait` body: poll the scanner until the selected workers
/// reach a terminal-for-the-orchestrator state (WaitingForInput / Question /
/// Inactive) or the timeout fires. `on_progress` is invoked at each progress
/// tick when a progress interval is supplied so the caller can print one
/// JSON line per tick.
pub fn worker_wait(
    targets: &[String],
    timeout: Duration,
    progress_interval: Option<Duration>,
    mut on_progress: impl FnMut(WaitProgress),
) -> WorkerWaitOutcome {
    let started = Instant::now();
    let deadline = started + timeout;

    let mut last_emit: Option<Instant> = None;

    let mut done: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::new();
    // A target that disappears from the scanner *after* having been seen
    // is treated as Inactive (worker tmux torn down). One that never
    // appears stays pending — fresh sessions sometimes lag the scanner.
    let mut ever_seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let timed_out = loop {
        let sessions = scanner::scan_sessions();
        for name in targets {
            if done.contains_key(name) {
                continue;
            }
            if let Some(s) = find_by_tmux(&sessions, name) {
                ever_seen.insert(name.clone());
                if matches!(
                    s.state,
                    models::SessionState::WaitingForInput
                        | models::SessionState::Question
                        | models::SessionState::Inactive
                ) {
                    done.insert(
                        name.clone(),
                        serde_json::json!({
                            "tmux": name,
                            "state": s.state.to_string(),
                            "done": true,
                            "last_user_message": s.last_user_message,
                        }),
                    );
                }
            } else if ever_seen.contains(name) {
                done.insert(
                    name.clone(),
                    serde_json::json!({
                        "tmux": name,
                        "state": models::SessionState::Inactive.to_string(),
                        "done": true,
                        "last_user_message": serde_json::Value::Null,
                    }),
                );
            }
        }
        if done.len() == targets.len() {
            break false;
        }
        if Instant::now() >= deadline {
            break true;
        }
        if let Some(interval) = progress_interval {
            let should_emit = match last_emit {
                None => true,
                Some(t) => t.elapsed() >= interval,
            };
            if should_emit {
                let mut done_names: Vec<String> = done.keys().cloned().collect();
                done_names.sort();
                let mut pending_names: Vec<String> = targets
                    .iter()
                    .filter(|n| !done.contains_key(n.as_str()))
                    .cloned()
                    .collect();
                pending_names.sort();
                on_progress(WaitProgress {
                    elapsed_secs: started.elapsed().as_secs(),
                    pending: pending_names,
                    done: done_names,
                });
                last_emit = Some(Instant::now());
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    };

    let elapsed_secs = started.elapsed().as_secs();
    let workers: Vec<serde_json::Value> = targets
        .iter()
        .map(|name| {
            done.get(name).cloned().unwrap_or_else(|| {
                serde_json::json!({
                    "tmux": name,
                    "state": "unknown",
                    "done": false,
                    "last_user_message": serde_json::Value::Null,
                })
            })
        })
        .collect();
    let all_done = done.len() == targets.len();

    WorkerWaitOutcome {
        all_done,
        timed_out,
        elapsed_secs,
        workers,
    }
}
