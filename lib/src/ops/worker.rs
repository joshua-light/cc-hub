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
    let project_root = state
        .require_project()
        .map_err(|e| OpError::Usage(e.to_string()))?
        .1
        .to_path_buf();

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
        spawn::spawn_agent_session(&agent_id, &cwd, None, initial_prompt, None, opts.readonly)
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
    let project_root = state
        .require_project()
        .map_err(|e| OpError::Usage(e.to_string()))?
        .1
        .to_path_buf();
    let branch = orchestrator::worktree_branch(task_id, worktree_name);
    let main = orchestrator::detect_main_branch(&project_root);

    // A task mid-Merging already holds the project merge lock from its own
    // `pr merge`. Because `merge_lock::acquire` is same-task idempotent, this
    // verb would "acquire" via refresh and then its unconditional release
    // below would drop the Merging phase's lock mid-/simplify — letting a
    // queued `pr merge` mutate main under the in-flight task. Refuse instead.
    if state.status == orchestrator::TaskStatus::Merging {
        return Err(OpError::conflict_with_recipe(
            format!(
                "task {} is mid-Merging (its `pr merge` holds the project merge lock)",
                task_id
            ),
            "Finish the PR flow with `cc-hub pr finalize` instead of `merge-worktree`.",
        ));
    }

    // Acquire the project merge lock so this legacy direct-merge path can't
    // race a concurrent `pr merge` into main. Non-blocking: a held lock means
    // another task owns the Merging phase right now. Only hand the lock a live
    // orchestrator tmux (see pr_merge / merge_lock::is_stale) so a dead session
    // doesn't make the lock look stale.
    let live_tmux = state
        .orchestrator_tmux
        .as_deref()
        .filter(|t| send::tmux_session_exists(t));
    match crate::merge_lock::acquire(project_id, task_id, live_tmux)
        .map_err(|e| OpError::Other(format!("acquire merge lock: {}", e)))?
    {
        crate::merge_lock::AcquireOutcome::Acquired => {}
        crate::merge_lock::AcquireOutcome::Held(holder) => {
            return Err(OpError::conflict_with_recipe(
                format!(
                    "merge in progress by task {} — the project merge lock is held",
                    holder.task_id
                ),
                "Wait for the in-flight merge to finish (`cc-hub pr finalize`), then re-run \
                 `cc-hub merge-worktree`.",
            ));
        }
    }

    // From here the lock is HELD: every return path must release it.
    // Capture the ref HEAD was on before `merge_branch` checks out `main`, so we
    // can restore it afterward instead of silently leaving the user on main.
    let prior_ref = capture_head_ref(&project_root);

    let (outcome, stdout, stderr) = match orchestrator::merge_branch(&project_root, &main, &branch) {
        Ok(v) => v,
        Err(e) => {
            let _ = crate::merge_lock::release(project_id, task_id);
            return Err(OpError::Other(format!("merge: {}", e)));
        }
    };

    // Restore HEAD to the pre-merge ref on a clean merge. On a conflict the
    // merge is left in progress on main for the user to resolve (a checkout
    // would fail anyway), and the dirty-tree preflight never moved HEAD — both
    // need no restore. Best-effort: a failure is a warning, not fatal.
    if matches!(outcome, MergeOutcome::Ok) {
        if let Some(r) = prior_ref.as_deref() {
            if r != main.as_str() {
                match orchestrator::run_git(&project_root, &["checkout", r]) {
                    Ok(o) if o.status_ok => {}
                    Ok(o) => log::warn!(
                        "merge-worktree: restore HEAD to {} failed: {}",
                        r,
                        o.stderr.trim()
                    ),
                    Err(e) => log::warn!("merge-worktree: restore HEAD to {} errored: {}", r, e),
                }
            }
        }
    }

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

    // Release on every terminal path — the direct merge owns the lock only for
    // the duration of this call (there's no `pr finalize` to release it later).
    let _ = crate::merge_lock::release(project_id, task_id);

    Ok(MergeWorktreeOutcome {
        outcome,
        worktree: worktree_name.to_string(),
        branch,
        main,
        stdout,
        stderr,
    })
}

/// The branch HEAD points to in `root`, or `None` when detached or git fails.
/// Used by [`merge_worktree`] to remember the user's branch before
/// `merge_branch` checks out `main`, so it can be restored afterward. (Mirrors
/// the private helper in `ops::pr`; kept local to avoid a cross-module coupling
/// for six lines.)
fn capture_head_ref(root: &std::path::Path) -> Option<String> {
    let out = orchestrator::run_git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok()?;
    if !out.status_ok {
        return None;
    }
    let name = out.stdout.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
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
        // Resolve to the NEWEST matching record: a retry pushes a fresh Worker
        // (new tmux) reusing the same worktree name, and the oldest record's
        // tmux is usually dead — waiting on it blocks the whole timeout.
        match state
            .workers
            .iter()
            .filter(|w| w.worktree.as_deref() == Some(wt.as_str()))
            .max_by_key(|w| w.spawned_at)
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
        // Dedup by identity keeping the NEWEST record: retries push a fresh
        // Worker (new tmux) on the same worktree, so a plain "every worker"
        // sweep waits on dead first-attempt sessions and blocks the whole
        // timeout. Identity is the worktree name (worktree workers) or the tmux
        // name (readonly workers, which have no worktree). First-appearance
        // order is preserved.
        use std::collections::HashMap;
        let mut slot: HashMap<String, usize> = HashMap::new();
        let mut chosen: Vec<(String, i64)> = Vec::new();
        for w in &state.workers {
            let key = match &w.worktree {
                Some(wt) => format!("wt:{}", wt),
                None => format!("tmux:{}", w.tmux_name),
            };
            match slot.get(&key) {
                Some(&i) => {
                    if w.spawned_at >= chosen[i].1 {
                        chosen[i] = (w.tmux_name.clone(), w.spawned_at);
                    }
                }
                None => {
                    slot.insert(key, chosen.len());
                    chosen.push((w.tmux_name.clone(), w.spawned_at));
                }
            }
        }
        for (tmux, _) in chosen {
            targets.push(tmux);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentKind;
    use std::path::PathBuf;

    fn worker(tmux: &str, worktree: Option<&str>, spawned_at: i64) -> Worker {
        Worker {
            agent_id: "claude".into(),
            agent_kind: AgentKind::Claude,
            tmux_name: tmux.into(),
            cwd: PathBuf::from("/tmp"),
            worktree: worktree.map(str::to_string),
            readonly: worktree.is_none(),
            spawned_at,
        }
    }

    fn state_with(workers: Vec<Worker>) -> TaskState {
        let mut s = TaskState::new("p".into(), PathBuf::from("/tmp/proj"), "do".into());
        s.task_id = "t".into();
        s.workers = workers;
        s
    }

    #[test]
    fn worktree_resolves_to_newest_matching_worker() {
        // A retry pushes a second record reusing the "fix" worktree; the older
        // record's tmux is usually dead, so resolve to the newest.
        let state = state_with(vec![
            worker("cchub-old", Some("fix"), 100),
            worker("cchub-new", Some("fix"), 200),
        ]);
        let targets = resolve_wait_targets(&state, &[], &["fix".into()], false).expect("ok");
        assert_eq!(targets, vec!["cchub-new".to_string()]);
    }

    #[test]
    fn all_dedups_by_worktree_keeping_newest() {
        let state = state_with(vec![
            worker("cchub-old", Some("fix"), 100),
            worker("cchub-new", Some("fix"), 200),
            worker("cchub-ro", None, 150),
        ]);
        let targets = resolve_wait_targets(&state, &[], &[], true).expect("ok");
        // One entry per identity: newest "fix" record, then the readonly worker.
        assert_eq!(
            targets,
            vec!["cchub-new".to_string(), "cchub-ro".to_string()]
        );
    }

    #[test]
    fn all_keeps_distinct_readonly_workers() {
        let state = state_with(vec![
            worker("cchub-ro1", None, 100),
            worker("cchub-ro2", None, 200),
        ]);
        let targets = resolve_wait_targets(&state, &[], &[], true).expect("ok");
        assert_eq!(
            targets,
            vec!["cchub-ro1".to_string(), "cchub-ro2".to_string()]
        );
    }
}
