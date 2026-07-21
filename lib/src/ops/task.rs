//! Task lifecycle ops.
//!
//! Bodies of `task create`, `task start`, `orchestrate start`, `task report`,
//! `task delete`, `task gc`, `task auto-review`, plus the artifact / todos
//! mutators. The CLI keeps flag parsing and JSON rendering; everything that
//! mutates on-disk task state lives here.

use std::path::PathBuf;
use std::time::Duration;

use crate::ops::worker::{wait_until_idle_and_send, PromptStatus, DEFAULT_PROMPT_WAIT_SECS};
use crate::ops::OpError;
use crate::orchestrator::{
    self, Artifact, DeletedTask, GcOutcome, TaskState, TaskStatus, TodoItem,
};
use crate::{config, spawn};

/// `cc-hub task create --prompt "..."` body. `project_id`/`project_root` are
/// resolved either from a registered `--project-id` (caller passes `Some`) or
/// from the cwd (caller passes `None`, and we register + derive here).
pub fn task_create(
    project_id: Option<&str>,
    name: Option<String>,
    prompt: String,
    backlog: bool,
) -> Result<TaskState, OpError> {
    let (project_id, project_root) = if let Some(id) = project_id {
        let projects = orchestrator::load_projects();
        let root = projects
            .projects
            .into_iter()
            .find(|p| p.id == id)
            .map(|p| p.root)
            .ok_or_else(|| {
                OpError::Usage(format!(
                    "--project-id {}: not registered in ~/.cc-hub/projects.toml",
                    id
                ))
            })?;
        (id.to_string(), root)
    } else {
        let cwd = std::env::current_dir().map_err(|e| OpError::Other(format!("cwd: {}", e)))?;
        let project_id = orchestrator::project_id_for_path(&cwd);
        let project_name = name.unwrap_or_else(|| {
            cwd.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| project_id.clone())
        });
        orchestrator::ensure_project_registered(&cwd, &project_name)
            .map_err(|e| OpError::Other(format!("register project: {}", e)))?;
        (project_id, cwd)
    };

    let state = if backlog {
        TaskState::new_backlog(project_id.clone(), project_root, prompt)
    } else {
        TaskState::new(project_id.clone(), project_root, prompt)
    };
    orchestrator::write_task_state(&state)
        .map_err(|e| OpError::Other(format!("write state: {}", e)))?;

    Ok(state)
}

/// Result of an orchestrator-spawn op ([`task_start`] / [`orchestrate_start`]).
pub struct OrchestratorSpawn {
    pub state: TaskState,
    pub tmux: String,
    pub prompt_status: PromptStatus,
}

/// `cc-hub task start` body: flip a Backlog task to Running, spawn its
/// orchestrator, and dispatch the orchestrator prompt.
pub fn task_start(
    project_id: &str,
    task_id: &str,
    agent: Option<String>,
    wait_secs: Option<u64>,
) -> Result<OrchestratorSpawn, OpError> {
    let (state, tmux_name, prompt) =
        orchestrator::start_backlog_task(project_id, task_id, agent.as_deref())
            .map_err(|e| OpError::Other(format!("start backlog task: {}", e)))?;

    let wait = wait_secs.unwrap_or(DEFAULT_PROMPT_WAIT_SECS);
    let prompt_status = if let Some(prompt) = prompt {
        match wait_until_idle_and_send(&tmux_name, &prompt, Duration::from_secs(wait)) {
            Ok(()) => PromptStatus::Sent,
            Err(e) => {
                log::warn!("task start: dispatch failed: {}", e);
                PromptStatus::Deferred(format!("prompt dispatch failed ({}), session is up", e))
            }
        }
    } else {
        PromptStatus::Sent
    };

    Ok(OrchestratorSpawn {
        state,
        tmux: tmux_name,
        prompt_status,
    })
}

/// Outcome of [`orchestrate_start`]: either a dry-run that resolved the prompt
/// without spawning, or a live spawn.
pub enum OrchestrateStart {
    /// `--dry-run`: the resolved orchestrator prompt, nothing spawned.
    DryRun(String),
    /// Orchestrator session spawned and prompt dispatched.
    /// Boxed: an inline [`OrchestratorSpawn`] dwarfs the `DryRun` variant
    /// (clippy::large_enum_variant).
    Spawned(Box<OrchestratorSpawn>),
}

/// `cc-hub orchestrate start` body: spawn the configured orchestrator backend
/// in the project root, persist its tmux session name, and dispatch the
/// generated orchestrator prompt. With `dry_run`, just resolves and returns
/// the prompt.
pub fn orchestrate_start(
    project_id: &str,
    task_id: &str,
    agent: Option<String>,
    wait_secs: Option<u64>,
    dry_run: bool,
) -> Result<OrchestrateStart, OpError> {
    let state = orchestrator::read_task_state(project_id, task_id)
        .map_err(|e| OpError::Other(format!("load state: {}", e)))?;

    let cc_hub_bin = orchestrator::resolve_cc_hub_bin();

    if dry_run {
        // Useful for verifying prompt content without paying for a session.
        let prompt = orchestrator::build_orchestrator_prompt(&state, &cc_hub_bin);
        return Ok(OrchestrateStart::DryRun(prompt));
    }

    let agent_id = agent
        .clone()
        .unwrap_or_else(|| config::get().default_orchestrator_agent_id());
    let agent = config::get()
        .agent(&agent_id)
        .ok_or_else(|| OpError::Other(format!("unknown orchestrator agent: {}", agent_id)))?;

    let cwd = state
        .require_project()
        .map_err(|e| OpError::Usage(e.to_string()))?
        .1
        .to_string_lossy()
        .into_owned();
    let prompt = orchestrator::build_orchestrator_prompt(&state, &cc_hub_bin);
    let initial_prompt = if agent.supports_initial_prompt() {
        Some(prompt.as_str())
    } else {
        None
    };
    let tmux_name = spawn::spawn_agent_session(&agent_id, &cwd, None, initial_prompt, None, false)
        .map_err(|e| OpError::Other(format!("spawn orchestrator: {}", e)))?;

    // Locked re-read + merge: the spawn takes long enough that writing the
    // pre-spawn snapshot back wholesale could clobber a concurrent update.
    let state = orchestrator::update_task_state(project_id, task_id, |s| {
        s.orchestrator_tmux = Some(tmux_name.clone());
        s.orchestrator_agent_id = agent_id.clone();
        s.orchestrator_agent_kind = agent.kind;
    })
    .map_err(|e| OpError::Other(format!("persist state: {}", e)))?;

    let wait = wait_secs.unwrap_or(DEFAULT_PROMPT_WAIT_SECS);
    let prompt_status = if agent.supports_initial_prompt() {
        PromptStatus::Sent
    } else {
        match wait_until_idle_and_send(&tmux_name, &prompt, Duration::from_secs(wait)) {
            Ok(()) => PromptStatus::Sent,
            Err(e) => {
                log::warn!("orchestrate start: dispatch failed: {}", e);
                PromptStatus::Deferred(format!("prompt dispatch failed ({}), session is up", e))
            }
        }
    };

    Ok(OrchestrateStart::Spawned(Box::new(OrchestratorSpawn {
        state,
        tmux: tmux_name,
        prompt_status,
    })))
}

/// Options for [`task_report`].
pub struct ReportOpts {
    pub status: Option<TaskStatus>,
    pub note: Option<String>,
    pub summary: Option<String>,
}

/// Result of [`task_report`]: the persisted state plus the caller-requested
/// status (which the CLI echoes back even when it differs from the effective
/// status, e.g. a `done` report routed to Review).
pub struct ReportOutcome {
    pub state: TaskState,
    pub requested_status: Option<TaskStatus>,
}

/// `cc-hub task report` body: apply the optional note/summary/status update,
/// routing a fresh `done` into Review, capturing `shipped_version` on the
/// first transition out of Running, re-arming the auto-reviewer on entry to
/// Review, and tearing down sessions when the task becomes terminal.
pub fn task_report(
    project_id: &str,
    task_id: &str,
    opts: ReportOpts,
) -> Result<ReportOutcome, OpError> {
    let raw_status = opts.status;
    let note = opts.note.clone();
    let summary = opts.summary.clone();

    // All rejection guards are evaluated against the LOCKED current status
    // inside the update closure (an unlocked pre-read raced the guards against
    // concurrent writers). The closure sets `rejection` and returns `false`,
    // which makes `try_update_task_state` abort WITHOUT writing — a refused
    // command must not bump `updated_at` and reshuffle the kanban.
    // `locked_prev` carries the pre-mutation status back out for the
    // terminal-cleanup check.
    let mut rejection: Option<OpError> = None;
    let mut locked_prev: Option<TaskStatus> = None;

    let (state, _written) = orchestrator::try_update_task_state(project_id, task_id, |s| {
        let prev = s.status;
        locked_prev = Some(prev);

        // Backlog is only a valid target from a Backlog state. Flipping a
        // running task to Backlog would hide it from the kanban while leaving
        // the orchestrator/tmux session alive — a zombie.
        if raw_status.as_ref() == Some(&TaskStatus::Backlog) && prev != TaskStatus::Backlog {
            rejection = Some(OpError::Usage(
                "--status backlog is only valid from a Backlog state".into(),
            ));
            return false;
        }

        // Symmetric guard: leaving Backlog requires an orchestrator spawn,
        // which only `task start` provides. A bare status flip would mutate
        // the on-disk state to e.g. Running without any tmux/session, leaving
        // a zombie that the `s` keybind can't recover (it requires Backlog).
        if raw_status.is_some()
            && prev == TaskStatus::Backlog
            && raw_status.as_ref() != Some(&TaskStatus::Backlog)
        {
            rejection = Some(OpError::Usage(
                "use cc-hub task start --task ID to launch a Backlog task; \
                 task report --status cannot spawn an orchestrator"
                    .into(),
            ));
            return false;
        }

        // A Merging task is mid-merge with the project merge lock held. A
        // `done`/`review` report here would route through `effective_status`
        // to Review (a legal Merging→Review edge reserved for conflict
        // demotion) and demote an already-merged task while the lock stays held
        // for up to STALE_TTL. Completing a merge goes through `pr finalize`,
        // not `task report`.
        if prev == TaskStatus::Merging
            && matches!(
                raw_status.as_ref(),
                Some(&TaskStatus::Done) | Some(&TaskStatus::Review)
            )
        {
            rejection = Some(OpError::Usage(format!(
                "task {} is Merging — run `cc-hub pr finalize` to complete the merge; \
                 `task report --status {}` can't finish it",
                task_id,
                raw_status.as_ref().map(TaskStatus::as_str).unwrap_or("")
            )));
            return false;
        }

        // `--status running` on a Review task with a live PR would silently
        // clobber the Review state the PR flow just established — a recurring
        // orchestrator mistake right after `pr create`. The sanctioned path
        // back to Running is `pr request-changes`. PR-less Review tasks keep
        // the direct path: they have no PR verb to do it for them.
        if raw_status.as_ref() == Some(&TaskStatus::Running) && prev == TaskStatus::Review {
            let live_pr = matches!(
                crate::pr::read_pr(project_id, task_id),
                Ok(Some(p)) if !matches!(
                    p.review_state,
                    crate::pr::ReviewState::Merged | crate::pr::ReviewState::Closed
                )
            );
            if live_pr {
                rejection = Some(OpError::Usage(
                    "task is in Review with a live PR — use `cc-hub pr request-changes` \
                     to send it back to Running (or `--status review` / no --status to \
                     report progress)"
                        .into(),
                ));
                return false;
            }
        }

        // An orchestrator's `--status done` means "I'm finished" — it does NOT
        // mean the work is approved. Route that into Review so a human (or
        // future agentic reviewer) signs off via the TUI's `Space` keybind.
        // The exception: if the task is already in Review, an explicit `done`
        // is the approval path — let it through.
        let effective_status = match (raw_status, prev) {
            (Some(TaskStatus::Done), p) if p != TaskStatus::Review => Some(TaskStatus::Review),
            (other, _) => other,
        };
        if let Some(st) = effective_status {
            s.status = st;
        }
        if let Some(note) = note.clone() {
            s.note = Some(note);
        }
        if let Some(summary) = summary.clone() {
            s.summary = Some(summary);
        }
        // Capture the project's shipped version on the *first* transition
        // out of Running. By this point the orchestrator's post-merge /bump
        // has already landed on the project's main branch, so the manifest
        // at `project_root` reflects the version that was just shipped.
        let leaving_running = prev == TaskStatus::Running
            && matches!(s.status, TaskStatus::Review | TaskStatus::Done);
        if leaving_running && s.shipped_version.is_none() {
            s.shipped_version = s.project_root.as_deref().and_then(crate::version::detect);
        }
        // Each transition *into* Review starts a fresh review round, so
        // the auto-reviewer gets one pass per round.
        if s.status == TaskStatus::Review && prev != TaskStatus::Review {
            s.last_auto_reviewed_at = None;
        }
        true
    })
    .map_err(|e| OpError::Other(format!("update state: {}", e)))?;

    if let Some(e) = rejection {
        return Err(e);
    }

    // Cleanup runs only when the task actually leaves the active flow:
    // Done is the only terminal state, and it's only reached via Review → Done
    // (fresh `done` reports go to Review and keep the orchestrator alive in
    // case the human wants follow-up).
    let became_terminal =
        state.status == TaskStatus::Done && locked_prev.as_ref() != Some(&TaskStatus::Done);
    if became_terminal {
        orchestrator::cleanup_task_sessions(&state);
    }

    Ok(ReportOutcome {
        state,
        requested_status: raw_status,
    })
}

/// `cc-hub task delete` body: kill the orchestrator tmux, remove the task's
/// worktrees, and delete the on-disk state dir. Requires `force` for active
/// (Running / Review / Merging) tasks; deleting a Merging task releases the
/// project merge lock.
pub fn task_delete(project_id: &str, task_id: &str, force: bool) -> Result<DeletedTask, OpError> {
    let state = match orchestrator::read_task_state(project_id, task_id) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(OpError::Other(format!(
                "no such task: {}/{}",
                project_id, task_id
            )));
        }
        Err(e) => return Err(OpError::Other(format!("load state: {}", e))),
    };

    // Active states require --force. Merging is included so the user has to
    // acknowledge they're tearing down an in-flight merge — but it CAN be
    // deleted (delete_task releases the merge lock below), which is the only
    // recovery path when the merging orchestrator has died.
    if matches!(
        state.status,
        TaskStatus::Running | TaskStatus::Review | TaskStatus::Merging
    ) && !force
    {
        return Err(OpError::Other(format!(
            "task {} is {}; pass --force to delete an active task (orchestrator tmux will be killed{})",
            task_id,
            state.status.as_str(),
            if state.status == TaskStatus::Merging {
                ", merge lock released"
            } else {
                ""
            }
        )));
    }

    orchestrator::delete_task(project_id, task_id)
        .map_err(|e| OpError::Other(format!("delete task: {}", e)))
}

/// `cc-hub task gc` body: sweep orphaned worktrees under
/// `<root>/.cc-hub-wt/`, delete their dangling `cc-hub/*` branches, and run
/// `git worktree prune`. `dry_run` returns the plan without acting.
pub fn task_gc(
    project_id: &str,
    project_root: &std::path::Path,
    dry_run: bool,
) -> Result<GcOutcome, OpError> {
    orchestrator::gc_worktrees(project_id, project_root, dry_run)
        .map_err(|e| OpError::Other(format!("gc worktrees: {}", e)))
}

/// `cc-hub task auto-review` body: re-arm the auto-reviewer for the current
/// Review round by clearing `last_auto_reviewed_at`. Errors unless the task is
/// in Review and has a PR in Open / ChangesRequested.
pub fn task_auto_review(project_id: &str, task_id: &str) -> Result<(), OpError> {
    let state = orchestrator::read_task_state(project_id, task_id)
        .map_err(|e| OpError::Other(format!("load state: {}", e)))?;
    if state.status != TaskStatus::Review {
        return Err(OpError::Usage(format!(
            "auto-review is only meaningful in the Review state (task is currently {})",
            state.status.as_str()
        )));
    }

    let pr = crate::pr::read_pr(project_id, task_id)
        .map_err(|e| OpError::Other(format!("read pr: {}", e)))?
        .ok_or_else(|| {
            OpError::Usage(
                "no PR exists for this task; auto-review only applies to a task with an open PR"
                    .into(),
            )
        })?;
    if !matches!(
        pr.review_state,
        crate::pr::ReviewState::Open | crate::pr::ReviewState::ChangesRequested
    ) {
        return Err(OpError::Usage(format!(
            "auto-review only applies to PRs in Open or ChangesRequested state (PR is currently {})",
            pr.review_state.as_str()
        )));
    }

    orchestrator::update_task_state(project_id, task_id, |s| {
        s.last_auto_reviewed_at = None;
    })
    .map_err(|e| OpError::Other(format!("persist state: {}", e)))?;

    Ok(())
}

// ─── task artifact ───────────────────────────────────────────────────────

fn looks_like_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Locked read-mutate-write routed by task flavor: `Some(pid)` hits the
/// project store, `None` the personal one.
fn update_task_for<F>(project_id: Option<&str>, task_id: &str, f: F) -> Result<TaskState, OpError>
where
    F: FnOnce(&mut TaskState),
{
    match project_id {
        Some(pid) => orchestrator::update_task_state(pid, task_id, f),
        None => orchestrator::update_personal_task(task_id, f),
    }
    .map_err(|e| OpError::Other(format!("persist state: {}", e)))
}

/// `cc-hub task artifact add` body (and the Tasks board's attach action):
/// copy a file (or record a URL) into the task's artifacts dir and append an
/// `Artifact` record. `project_id: None` addresses a personal-board task.
/// Returns the persisted state so the caller can echo the added artifact +
/// lead index.
pub fn task_artifact_add(
    project_id: Option<&str>,
    task_id: &str,
    raw_path: &str,
    kind: Option<String>,
    caption: Option<String>,
    lead: bool,
) -> Result<TaskState, OpError> {
    // Confirm the task exists before doing any filesystem work, so we don't
    // copy files into a directory that points at a nonexistent task.
    let _ = orchestrator::read_task_state_for(project_id, task_id)
        .map_err(|e| OpError::Other(format!("load state: {}", e)))?;

    let (kind, stored_path) = if looks_like_url(raw_path) {
        let kind = kind.unwrap_or_else(|| "url".into());
        (kind, raw_path.to_string())
    } else {
        let kind = kind.unwrap_or_else(|| "file".into());
        let src = std::fs::canonicalize(raw_path).map_err(|e| {
            OpError::Other(format!(
                "resolve source path {:?}: {} (does the file exist?)",
                raw_path, e
            ))
        })?;
        let meta = std::fs::metadata(&src)
            .map_err(|e| OpError::Other(format!("stat {}: {}", src.display(), e)))?;
        if meta.is_dir() {
            return Err(OpError::Other(format!(
                "{} is a directory; only single files are supported",
                src.display()
            )));
        }
        let basename = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| OpError::Other(format!("{} has no file name", src.display())))?;

        let dest_dir = orchestrator::task_dir_for(project_id, task_id)
            .ok_or_else(|| OpError::Other("no home dir".into()))?
            .join("artifacts");
        std::fs::create_dir_all(&dest_dir)
            .map_err(|e| OpError::Other(format!("create {}: {}", dest_dir.display(), e)))?;

        let ts = orchestrator::now_unix_secs();
        let dest = dest_dir.join(format!("{}-{}", ts, basename));
        std::fs::copy(&src, &dest).map_err(|e| {
            OpError::Other(format!(
                "copy {} -> {}: {}",
                src.display(),
                dest.display(),
                e
            ))
        })?;
        (kind, dest.to_string_lossy().into_owned())
    };

    let artifact = Artifact {
        kind: kind.clone(),
        path: stored_path.clone(),
        original: raw_path.to_string(),
        caption,
        added_at: orchestrator::now_unix_secs(),
    };
    let mark_lead = lead;
    update_task_for(project_id, task_id, |s| {
        s.artifacts.push(artifact.clone());
        if mark_lead {
            s.lead_artifact = Some(s.artifacts.len() - 1);
        }
    })
}

/// `cc-hub task artifact list` body: load the task state for its artifacts.
/// `project_id: None` addresses a personal-board task.
pub fn task_artifact_list(project_id: Option<&str>, task_id: &str) -> Result<TaskState, OpError> {
    orchestrator::read_task_state_for(project_id, task_id)
        .map_err(|e| OpError::Other(format!("load state: {}", e)))
}

/// Attach pasted text to a task: write it as a fresh `<ts>-note.md` inside
/// the task's artifacts dir (there is no source file to copy) and append a
/// `note` Artifact record whose caption is the text's first line, so the
/// card header says what the note is about. `original` is `"clipboard"` —
/// the text never had a path. Returns the persisted state.
pub fn task_artifact_add_text(
    project_id: Option<&str>,
    task_id: &str,
    text: &str,
) -> Result<TaskState, OpError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(OpError::Usage("no text to attach".into()));
    }
    // Confirm the task exists before touching the filesystem, mirroring
    // `task_artifact_add`.
    let _ = orchestrator::read_task_state_for(project_id, task_id)
        .map_err(|e| OpError::Other(format!("load state: {}", e)))?;

    let dest_dir = orchestrator::task_dir_for(project_id, task_id)
        .ok_or_else(|| OpError::Other("no home dir".into()))?
        .join("artifacts");
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| OpError::Other(format!("create {}: {}", dest_dir.display(), e)))?;

    // Every note shares the `note.md` basename, so a same-second double
    // paste would silently overwrite (and removal of one record would
    // delete the other's file) — probe for a free name instead.
    let ts = orchestrator::now_unix_secs();
    let mut dest = dest_dir.join(format!("{}-note.md", ts));
    let mut n = 2;
    while dest.exists() {
        dest = dest_dir.join(format!("{}-note-{}.md", ts, n));
        n += 1;
    }
    std::fs::write(&dest, text)
        .map_err(|e| OpError::Other(format!("write {}: {}", dest.display(), e)))?;

    let artifact = Artifact {
        kind: "note".into(),
        path: dest.to_string_lossy().into_owned(),
        original: "clipboard".into(),
        caption: Some(crate::models::first_line_truncated(trimmed, 48)),
        added_at: ts,
    };
    update_task_for(project_id, task_id, |s| s.artifacts.push(artifact.clone()))
}

/// Remove artifact `index` from a personal-board task: drop the record (fixing
/// `lead_artifact` if it pointed at or past the removed slot) and best-effort
/// delete the stored copy — but only when it lives inside the task's own
/// artifacts dir, so a URL or a hand-attached external path is never touched.
/// Personal-only by design: orchestrated artifacts stay append-only as
/// proof-of-work evidence. Returns the persisted state plus the removed record.
pub fn task_artifact_remove(task_id: &str, index: usize) -> Result<(TaskState, Artifact), OpError> {
    let state = orchestrator::read_task_state_for(None, task_id)
        .map_err(|e| OpError::Other(format!("load state: {}", e)))?;
    if index >= state.artifacts.len() {
        return Err(OpError::NotFound(format!(
            "task {} has no artifact #{}",
            task_id, index
        )));
    }
    // State first, file second: a crash in between leaves an orphan file
    // (harmless), while the reverse order would leave a record pointing at
    // nothing.
    let mut removed: Option<Artifact> = None;
    let state = update_task_for(None, task_id, |s| {
        if index < s.artifacts.len() {
            removed = Some(s.artifacts.remove(index));
            s.lead_artifact = match s.lead_artifact {
                Some(lead) if lead == index => None,
                Some(lead) if lead > index => Some(lead - 1),
                other => other,
            };
        }
    })?;
    let removed = removed.ok_or_else(|| OpError::Conflict {
        msg: format!(
            "artifact #{} vanished before removal (concurrent edit?)",
            index
        ),
        recipe: None,
    })?;

    let artifacts_dir = orchestrator::task_dir_for(None, task_id)
        .ok_or_else(|| OpError::Other("no home dir".into()))?
        .join("artifacts");
    let stored = PathBuf::from(&removed.path);
    if stored.starts_with(&artifacts_dir) {
        if let Err(e) = std::fs::remove_file(&stored) {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!(
                    "artifact remove: file cleanup failed for {}: {}",
                    removed.path,
                    e
                );
            }
        }
    }
    Ok((state, removed))
}

// ─── task todos ──────────────────────────────────────────────────────────

/// `cc-hub task todos set` body: replace the checklist from newline-separated
/// texts.
pub fn task_todos_set(
    project_id: &str,
    task_id: &str,
    raw_items: &str,
) -> Result<TaskState, OpError> {
    let new_todos: Vec<TodoItem> = raw_items
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| TodoItem {
            text: l.to_string(),
            done: false,
        })
        .collect();

    orchestrator::update_task_state(project_id, task_id, |s| {
        s.todos = new_todos;
    })
    .map_err(|e| OpError::Other(format!("persist state: {}", e)))
}

/// `cc-hub task todos check|uncheck` body: flip the done flag on one todo.
pub fn task_todos_mark(
    project_id: &str,
    task_id: &str,
    idx: usize,
    done: bool,
) -> Result<TaskState, OpError> {
    let pre = orchestrator::read_task_state(project_id, task_id)
        .map_err(|e| OpError::Other(format!("load state: {}", e)))?;
    if idx >= pre.todos.len() {
        return Err(OpError::Usage(format!(
            "--index {} out of range (have {} todo{})",
            idx,
            pre.todos.len(),
            if pre.todos.len() == 1 { "" } else { "s" }
        )));
    }

    orchestrator::update_task_state(project_id, task_id, |s| {
        if let Some(t) = s.todos.get_mut(idx) {
            t.done = done;
        }
    })
    .map_err(|e| OpError::Other(format!("persist state: {}", e)))
}

/// `cc-hub task todos clear` body: empty the checklist.
pub fn task_todos_clear(project_id: &str, task_id: &str) -> Result<TaskState, OpError> {
    orchestrator::update_task_state(project_id, task_id, |s| {
        s.todos.clear();
    })
    .map_err(|e| OpError::Other(format!("persist state: {}", e)))
}

/// Locate the worktree directory for `branch` by checking the task's
/// recorded workers. Falls back to the conventional `<root>/.cc-hub-wt/`
/// path layout if no Worker record matches. Shared with [`crate::ops::pr`].
pub(crate) fn resolve_worktree_path(state: &TaskState, branch: &str) -> Option<PathBuf> {
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
    Some(orchestrator::worktree_path(
        state.project_root.as_deref()?,
        &state.task_id,
        name,
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::orchestrator::TaskState;
    use std::path::PathBuf;

    fn seed(project_id: &str, task_id: &str, status: TaskStatus) {
        let mut state = TaskState::new(
            project_id.into(),
            PathBuf::from("/tmp/proj"),
            "do thing".into(),
        );
        state.task_id = task_id.into();
        state.status = status;
        orchestrator::write_task_state(&state).expect("write state");
    }

    fn report(
        project_id: &str,
        task_id: &str,
        status: Option<TaskStatus>,
    ) -> Result<ReportOutcome, OpError> {
        task_report(
            project_id,
            task_id,
            ReportOpts {
                status,
                note: None,
                summary: None,
            },
        )
    }

    #[test]
    fn report_done_while_merging_is_rejected_pointing_at_finalize() {
        // BUG 6: `--status done` while Merging must NOT demote the merged task
        // to Review (which would strand the held merge lock) — reject it and
        // point at `pr finalize`.
        crate::test_util::with_temp_home(|| {
            let (p, t) = ("p-merging", "t-merging");
            seed(p, t, TaskStatus::Merging);
            match report(p, t, Some(TaskStatus::Done)) {
                Err(OpError::Usage(msg)) => {
                    assert!(msg.contains("pr finalize"), "message: {}", msg)
                }
                Err(other) => panic!("expected Usage, got {:?}", other),
                Ok(_) => panic!("done while Merging must be rejected"),
            }
            let after = orchestrator::read_task_state(p, t).expect("read state");
            assert_eq!(
                after.status,
                TaskStatus::Merging,
                "a merged task must not be demoted by a `done` report"
            );
        });
    }

    #[test]
    fn report_review_while_merging_is_rejected() {
        crate::test_util::with_temp_home(|| {
            let (p, t) = ("p-merging2", "t-merging2");
            seed(p, t, TaskStatus::Merging);
            match report(p, t, Some(TaskStatus::Review)) {
                Err(OpError::Usage(_)) => {}
                Err(other) => panic!("expected Usage, got {:?}", other),
                Ok(_) => panic!("review while Merging must be rejected"),
            }
            let after = orchestrator::read_task_state(p, t).expect("read state");
            assert_eq!(after.status, TaskStatus::Merging);
        });
    }

    #[test]
    fn report_progress_note_while_merging_is_allowed() {
        crate::test_util::with_temp_home(|| {
            let (p, t) = ("p-merging3", "t-merging3");
            seed(p, t, TaskStatus::Merging);
            // No --status, just a note: must pass and stay Merging.
            let out = task_report(
                p,
                t,
                ReportOpts {
                    status: None,
                    note: Some("still merging".into()),
                    summary: None,
                },
            )
            .expect("progress note allowed while Merging");
            assert_eq!(out.state.status, TaskStatus::Merging);
            assert_eq!(out.state.note.as_deref(), Some("still merging"));
        });
    }

    #[test]
    fn report_fresh_done_from_running_routes_to_review() {
        crate::test_util::with_temp_home(|| {
            let (p, t) = ("p-fresh-done", "t-fresh-done");
            seed(p, t, TaskStatus::Running);
            let out = report(p, t, Some(TaskStatus::Done)).expect("done from Running ok");
            // Fresh `done` routes to Review for sign-off, not straight to Done.
            assert_eq!(out.state.status, TaskStatus::Review);
            assert_eq!(out.requested_status, Some(TaskStatus::Done));
        });
    }
}
