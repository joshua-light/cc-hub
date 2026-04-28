//! Strategic / Projects layer.
//!
//! cc-hub today owns Claude Code sessions one-by-one. The orchestrator layer
//! sits one level higher: a **project** is a registered directory (usually a
//! git repo root); a **task** is a high-level user request handled by a
//! single **orchestrator session** which spawns and manages worker sessions.
//!
//! State for each task lives at
//! `~/.cc-hub/projects/<project-id>/tasks/<task-id>/state.json` and is
//! written by the orchestrator (via `cc-hub task report`, `cc-hub
//! spawn-worker`, `cc-hub merge-worktree`) and read by the TUI's Projects
//! view. The list of registered projects lives at `~/.cc-hub/projects.toml`.
//!
//! This module owns only the schema + on-disk helpers. The CLI subcommands
//! that mutate it live in `bin/src/cli.rs`; the TUI consumer lives in
//! `lib/src/ui.rs`.
//!
//! Project ID derivation: canonical path with non-alphanumeric runs collapsed
//! to single dashes. Stable, human-readable, no hashing dep needed. Two
//! different paths can in theory collide (e.g. `/foo/bar` and `/foo-bar`),
//! but in practice every project is a real filesystem path so collisions
//! require deliberate construction.
//!
//! Task ID format: `t-<unix-nanos>`. Sortable, unique within a single host
//! to nanosecond resolution, no extra dep.
//!
//! Worktree convention: `<project-root>/.cc-hub-wt/<task-id>-<name>` off
//! `main`. The orchestrator picks `<name>`; cc-hub creates the directory and
//! the branch.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Root of cc-hub's user state. `~/.cc-hub/`. None when home is unresolvable.
pub fn cc_hub_home() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cc-hub"))
}

pub fn projects_toml_path() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("projects.toml"))
}

pub fn projects_state_dir() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("projects"))
}

pub fn project_state_dir(project_id: &str) -> Option<PathBuf> {
    projects_state_dir().map(|d| d.join(project_id))
}

pub fn task_state_dir(project_id: &str, task_id: &str) -> Option<PathBuf> {
    project_state_dir(project_id).map(|d| d.join("tasks").join(task_id))
}

pub fn task_state_file(project_id: &str, task_id: &str) -> Option<PathBuf> {
    task_state_dir(project_id, task_id).map(|d| d.join("state.json"))
}

pub fn task_orchestrator_log_path(project_id: &str, task_id: &str) -> Option<PathBuf> {
    task_state_dir(project_id, task_id).map(|d| d.join("orchestrator.log"))
}

/// Compute a stable, human-readable project id from a filesystem path. The
/// path is canonicalised when possible; symlink targets normalise to the same
/// id as the symlink itself.
pub fn project_id_for_path(root: &Path) -> String {
    let canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let raw = canon.to_string_lossy();
    let mut id = String::with_capacity(raw.len());
    let mut last_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            id.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            id.push('-');
            last_dash = true;
        }
    }
    let trimmed = id.trim_matches('-');
    if trimmed.is_empty() {
        "root".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn new_task_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("t-{}", nanos)
}

/// Compact rendering of a `t-<unix-nanos>` id for in-card display. Last 6
/// digits are unique within the active set without dominating the badge.
pub fn short_task_id(task_id: &str) -> String {
    let trimmed = task_id.trim_start_matches("t-");
    let take = trimmed.len().saturating_sub(6);
    trimmed[take..].to_string()
}

pub fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Backlog,
    Running,
    /// Orchestrator finished its work and is waiting on a human (or future
    /// agentic reviewer) to sign off. `Done` is reached only via explicit
    /// approval. New `done` task-state files written by older orchestrators
    /// land directly in `Done` for back-compat — only newly-completed tasks
    /// get parked here.
    Review,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worker {
    pub tmux_name: String,
    pub cwd: PathBuf,
    pub worktree: Option<String>,
    pub readonly: bool,
    pub spawned_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MergeOutcome {
    Ok,
    Conflict { detail: String },
    /// Preflight refused: another orchestrator's task holds an `active`
    /// reservation overlapping this branch's changed paths. The blocking
    /// task id and the overlapping path set are surfaced so the caller can
    /// either wait or escalate. No working-tree mutation happens for this
    /// outcome.
    BlockedByActiveOrchestrator {
        task_id: String,
        paths: Vec<String>,
    },
    /// Pre-flight refused: the working tree on the target branch has
    /// uncommitted edits in files the feature branch also modified, so
    /// the merge would either fail with "would be overwritten" or — worse
    /// — be auto-stashed and produce conflict markers on pop. We detect
    /// this up front and decline to touch the tree. `overlap` lists the
    /// repo-relative paths the user must commit, stash, or revert before
    /// retrying. Distinct from `Conflict`, which means git started the
    /// merge and hit content conflicts during it.
    BlockedByDirtyTree { overlap: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeRecord {
    pub worktree: String,
    pub at: i64,
    pub outcome: MergeOutcome,
}

/// A piece of evidence the orchestrator (or a worker) attached to a task —
/// screenshot, log, build output, URL, etc. Stored alongside the task state
/// so it survives worktree cleanup. `kind` is free-form by design; the CLI
/// suggests common values but doesn't constrain them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub kind: String,
    /// Absolute path to the copied-into-state file, OR the original URL when
    /// `kind == "url"` (or any URL-shaped path).
    pub path: String,
    /// User-supplied path/url, preserved so consumers can show where the
    /// artifact originated even after cc-hub has copied it into its store.
    pub original: String,
    pub caption: Option<String>,
    pub added_at: i64,
}

/// One entry in the orchestrator's optional plan checklist. Surfaced on the
/// active task card as `done/total ✓`. Free-form text — no Markdown rendering
/// in the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub text: String,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskState {
    pub task_id: String,
    pub project_id: String,
    pub project_root: PathBuf,
    /// Filled in by the orchestrator the first time it reports — cc-hub
    /// can't know its session id at spawn time (Claude generates it).
    #[serde(default)]
    pub orchestrator_session_id: Option<String>,
    /// tmux session name hosting the orchestrator. Set by `orchestrate
    /// start` immediately after spawn so the TUI / scanner can group
    /// child workers under the right parent without waiting for the
    /// orchestrator to self-report.
    #[serde(default)]
    pub orchestrator_tmux: Option<String>,
    pub status: TaskStatus,
    /// Free-form prompt the user submitted when creating the task. Frozen
    /// after creation; the orchestrator sees it via its system prompt.
    pub prompt: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// One-line latest status from the orchestrator. Surface in the
    /// Projects view so the user can skim a project at a glance.
    #[serde(default)]
    pub note: Option<String>,
    /// Multi-line proof-of-work summary written by the orchestrator on
    /// completion. Distinct from `note`, which is the latest one-line
    /// status. `serde(default)` so older state.json files still load.
    #[serde(default)]
    pub summary: Option<String>,
    /// 2-3 word Haiku-generated title for the task, derived from the user
    /// prompt. Mirrors `SessionInfo::title`. Persisted; `None` until the
    /// background titler finishes (or if it fails).
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub workers: Vec<Worker>,
    #[serde(default)]
    pub merges: Vec<MergeRecord>,
    /// Proof-of-work artifacts attached over the task's lifetime. Append
    /// only via the CLI; `serde(default)` for back-compat.
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    /// Optional orchestrator-maintained checklist. Empty for tasks where the
    /// orchestrator never opted in. `serde(default)` so older state.json
    /// files still load.
    #[serde(default)]
    pub todos: Vec<TodoItem>,
    /// Index into `artifacts` of the single "lead" artifact — the strongest
    /// piece of proof, surfaced first when the user reviews the task. The
    /// agent designates it via `task artifact add --lead`. `None` until set;
    /// re-passing `--lead` moves the designation. `serde(default)` for
    /// back-compat with state files written before this field existed.
    #[serde(default)]
    pub lead_artifact: Option<usize>,
    /// Version of the project that was shipped as a result of this task,
    /// captured at the moment the orchestrator first declares completion
    /// (Running → Review/Done/Failed). Read from the project's manifest
    /// (Cargo.toml / package.json / pyproject.toml / VERSION) in the project
    /// root, which by that point reflects any /bump commit the orchestrator
    /// just landed. `None` if the project has no recognised manifest, or if
    /// the task never transitioned out of Running. `serde(default)` for
    /// back-compat with state.json files written before this field existed.
    #[serde(default)]
    pub shipped_version: Option<String>,
}

impl TaskState {
    pub fn new(project_id: String, project_root: PathBuf, prompt: String) -> Self {
        let now = now_unix_secs();
        Self {
            task_id: new_task_id(),
            project_id,
            project_root,
            orchestrator_session_id: None,
            orchestrator_tmux: None,
            status: TaskStatus::Running,
            prompt,
            created_at: now,
            updated_at: now,
            note: None,
            summary: None,
            title: None,
            workers: Vec::new(),
            merges: Vec::new(),
            artifacts: Vec::new(),
            todos: Vec::new(),
            lead_artifact: None,
            shipped_version: None,
        }
    }

    pub fn new_backlog(project_id: String, project_root: PathBuf, prompt: String) -> Self {
        let now = now_unix_secs();
        Self {
            task_id: new_task_id(),
            project_id,
            project_root,
            orchestrator_session_id: None,
            orchestrator_tmux: None,
            status: TaskStatus::Backlog,
            prompt,
            created_at: now,
            updated_at: now,
            note: None,
            summary: None,
            title: None,
            workers: Vec::new(),
            merges: Vec::new(),
            artifacts: Vec::new(),
            todos: Vec::new(),
            lead_artifact: None,
            shipped_version: None,
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = now_unix_secs();
    }
}

/// Read a task state file; missing file returns NotFound, parse errors
/// surface as InvalidData so callers can distinguish "no such task" from
/// "schema drift".
pub fn read_task_state(project_id: &str, task_id: &str) -> io::Result<TaskState> {
    let path = task_state_file(project_id, task_id)
        .ok_or_else(|| io::Error::other("no home dir"))?;
    let raw = fs::read_to_string(&path)?;
    serde_json::from_str(&raw)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {}", path.display(), e)))
}

/// Atomically write a task state file via tempfile + rename. Creates parent
/// dirs on demand.
pub fn write_task_state(state: &TaskState) -> io::Result<()> {
    let path = task_state_file(&state.project_id, &state.task_id)
        .ok_or_else(|| io::Error::other("no home dir"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(state)
        .map_err(|e| io::Error::other(format!("serialize state: {}", e)))?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        use std::io::Write;
        let mut f = fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// In-place update under `read → mutate → write`. The closure receives a
/// mutable state and is responsible for any field-level changes; `touch()`
/// is called automatically after the closure so callers don't have to
/// remember.
pub fn update_task_state<F>(project_id: &str, task_id: &str, f: F) -> io::Result<TaskState>
where
    F: FnOnce(&mut TaskState),
{
    let mut state = read_task_state(project_id, task_id)?;
    f(&mut state);
    state.touch();
    write_task_state(&state)?;
    Ok(state)
}

/// Persist a Haiku-generated short title onto a task's state file. Reuses
/// the per-task atomic-write store rather than a side cache file so the
/// title travels with the rest of the task state.
pub fn set_task_title(
    project_id: &str,
    task_id: &str,
    title: &str,
) -> io::Result<TaskState> {
    update_task_state(project_id, task_id, |s| {
        s.title = Some(title.to_string());
    })
}

/// Tear down every tmux session associated with a finished task: workers
/// immediately, orchestrator after a short delay. The orchestrator is
/// almost always the calling process when this runs from the CLI (a Claude
/// session running this CLI via Bash), so killing its tmux synchronously
/// would terminate the caller before its JSON output is captured. The
/// detached `sh -c` keeps the kill alive past our exit.
///
/// Called from two places: the CLI (`task report` when status flips to a
/// terminal state) and the TUI (when a human approves a Review task). Both
/// need the same behaviour, so it lives in lib.
pub fn cleanup_task_sessions(state: &TaskState) {
    if let Some(orch) = state.orchestrator_tmux.as_deref() {
        capture_orchestrator_log(state, orch);
    }
    for w in &state.workers {
        if let Err(e) = crate::send::kill_tmux_session(&w.tmux_name) {
            log::warn!(
                "task {}: kill worker tmux [{}] failed: {}",
                state.task_id, w.tmux_name, e
            );
        }
    }
    if let Some(orch) = state.orchestrator_tmux.as_deref() {
        // tmux session names from `spawn_claude_session` are alphanumeric +
        // `-`/`_`/`.`. Anything else is suspicious — skip rather than risk
        // shell injection in the detached killer.
        let safe_name: String = orch
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .collect();
        if safe_name != orch {
            log::warn!(
                "task {}: orchestrator tmux name [{}] has unexpected chars; not scheduling kill",
                state.task_id, orch
            );
            return;
        }
        let cmd = format!("sleep 2; tmux kill-session -t {} 2>/dev/null", safe_name);
        match std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => log::info!(
                "task {}: scheduled orchestrator tmux [{}] kill in 2s",
                state.task_id, orch
            ),
            Err(e) => log::warn!(
                "task {}: schedule orchestrator kill failed: {}",
                state.task_id, e
            ),
        }
    }
}

fn capture_orchestrator_log(state: &TaskState, orch: &str) {
    let Some(path) = task_orchestrator_log_path(&state.project_id, &state.task_id) else {
        return;
    };
    let Some(dir) = path.parent() else { return };
    if let Err(e) = std::fs::create_dir_all(dir) {
        log::warn!(
            "task {}: orchestrator.log mkdir failed: {}",
            state.task_id, e
        );
        return;
    }
    let body = crate::send::capture_tmux_pane_full(orch);
    if body.is_empty() {
        log::warn!(
            "task {}: orchestrator capture-pane returned empty for [{}]",
            state.task_id, orch
        );
        return;
    }
    if let Err(e) = std::fs::write(&path, body) {
        log::warn!(
            "task {}: write orchestrator.log failed: {}",
            state.task_id, e
        );
    } else {
        log::info!(
            "task {}: wrote orchestrator log to {}",
            state.task_id,
            path.display()
        );
    }
}

/// One registered project. Stored in `~/.cc-hub/projects.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectsFile {
    #[serde(default, rename = "project")]
    pub projects: Vec<Project>,
}

pub fn load_projects() -> ProjectsFile {
    let Some(path) = projects_toml_path() else {
        return ProjectsFile::default();
    };
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return ProjectsFile::default(),
        Err(e) => {
            log::warn!("projects.toml: read error at {}: {}", path.display(), e);
            return ProjectsFile::default();
        }
    };
    match toml::from_str::<ProjectsFile>(&raw) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("projects.toml: parse error at {}: {}", path.display(), e);
            ProjectsFile::default()
        }
    }
}

pub fn save_projects(file: &ProjectsFile) -> io::Result<()> {
    let path = projects_toml_path().ok_or_else(|| io::Error::other("no home dir"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = toml::to_string_pretty(file)
        .map_err(|e| io::Error::other(format!("serialize projects.toml: {}", e)))?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        use std::io::Write;
        let mut f = fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Register `root` if it isn't already, returning the project id either
/// way. `name` is used only when inserting a new entry.
pub fn ensure_project_registered(root: &Path, name: &str) -> io::Result<String> {
    let id = project_id_for_path(root);
    let mut file = load_projects();
    if !file.projects.iter().any(|p| p.id == id) {
        let canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        file.projects.push(Project {
            id: id.clone(),
            name: name.to_string(),
            root: canon,
            created_at: now_unix_secs(),
        });
        save_projects(&file)?;
    }
    Ok(id)
}

/// Remove a registered project from `~/.cc-hub/projects.toml` and delete
/// its on-disk task state directory (`~/.cc-hub/projects/<id>`). Returns
/// `Ok(())` if the project was removed (or wasn't present), or an error if
/// any orchestrator for this project is still alive — the caller surfaces
/// that to the user so they can clean up tasks first.
pub fn remove_project(project_id: &str) -> io::Result<()> {
    let mut file = load_projects();
    if !file.projects.iter().any(|p| p.id == project_id) {
        return Ok(());
    }

    let proj_dir = project_state_dir(project_id);
    let tasks_dir = proj_dir.as_ref().map(|d| d.join("tasks"));
    if let Some(tasks_dir) = tasks_dir.as_ref() {
        if tasks_dir.is_dir() {
            for entry in fs::read_dir(tasks_dir)? {
                let entry = entry?;
                let state_path = entry.path().join("state.json");
                let raw = match fs::read_to_string(&state_path) {
                    Ok(s) => s,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e),
                };
                let state: TaskState = match serde_json::from_str(&raw) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if let Some(orch) = state.orchestrator_tmux.as_deref() {
                    if crate::send::tmux_session_exists(orch) {
                        return Err(io::Error::other(format!(
                            "refusing: orchestrator {} still alive for task {}",
                            orch, state.task_id
                        )));
                    }
                }
            }
        }
    }

    file.projects.retain(|p| p.id != project_id);
    save_projects(&file)?;
    if let Some(dir) = proj_dir.as_ref() {
        if dir.exists() {
            if let Err(e) = fs::remove_dir_all(dir) {
                log::warn!(
                    "remove_project {}: rm -rf {} failed: {}",
                    project_id,
                    dir.display(),
                    e
                );
            }
        }
    }
    Ok(())
}

/// First user message dispatched to a freshly-spawned orchestrator session.
///
/// This is *the* contract between cc-hub and the orchestrator role: it
/// teaches a stock Claude Code session about the four CLI primitives, sets
/// expectations (decompose, don't impl; parallelize reads; serialize edits),
/// and embeds the user's actual task verbatim. Keep it concise — the
/// orchestrator pays for it on every turn.
///
/// `cc_hub_bin` is the absolute path to the cc-hub binary running this
/// process — pre-substituted into every example so the orchestrator's Bash
/// shell doesn't need cc-hub on `PATH` (a real failure mode observed in
/// the first end-to-end run, where the orch had to guess the path).
pub fn build_orchestrator_prompt(state: &TaskState, cc_hub_bin: &Path) -> String {
    let TaskState {
        task_id,
        project_id,
        project_root,
        prompt,
        ..
    } = state;
    let bin = cc_hub_bin.display();
    format!(
        "You are the cc-hub orchestrator for task `{task_id}` in project `{project_id}` at `{root}`.

Your job is to decompose, dispatch, monitor, and merge — not to write code yourself. Run cc-hub subcommands via the Bash tool to spawn worker sessions and track progress. Keep your own session focused on coordination.

The cc-hub binary is at `{bin}`. Always invoke it by absolute path; it is not necessarily on PATH inside worker shells.

# How to work

1. **Plan.** Break the task into sub-tasks. Note which can run in parallel (read-only, or edits to disjoint files) and which must run serially.

1b. **Declare intent.** Before spawning any *worktree* workers (read-only workers don't reserve), declare your best-guess file set so other orchestrators can coordinate:
   `{bin} reservations declare --task {task_id} --phase intended --paths <files>`
   Use directory prefixes like `lib/src/` if uncertain — you can widen later with `--add-paths`. Then read the project's reservation table:
   `{bin} reservations list --project {project_id}`
   Reconcile based on what you see (stale entries are filtered out by `list` automatically; treat as cleared):
   - **Another task is `active` on overlapping paths** — yield. Don't block idle: front-load read-only research subtasks (those need no reservation) and re-poll every minute or so. This \"research while waiting\" pattern is the whole point of declaring early — keep yourself useful while the other task finishes.
   - **Another `intended` task overlaps your paths** — FIFO on `created_at`. If they declared first, yield (same research-while-waiting pattern). If you did, proceed.

2. **Dispatch workers.** Two modes:
   - `{bin} spawn-worker --task {task_id} --readonly --prompt \"…\"` — read/research tasks. No edits, no worktree, runs in the project root. Many can run at once.
   - **Before each worktree spawn, upgrade your reservation to `active`:**
     `{bin} reservations upgrade --task {task_id} --to active --worker-paths <files>`
     If upgrade fails, another orchestrator beat you to those files in the critical section — reconcile (re-read `{bin} reservations list --project {project_id}`) and retry once the conflict clears.
   - `{bin} spawn-worker --task {task_id} --worktree NAME --prompt \"…\"` — editing tasks. cc-hub creates a fresh worktree at `.cc-hub-wt/{task_id}-NAME` on a new branch off the project's main branch. Two worktree workers can run in parallel **only** if they edit disjoint sets of files; otherwise serialize them.
   Each spawn-worker emits one JSON line on stdout — capture the `tmux` field if you need to interact with that worker later.

3. **Monitor.** Two reliable channels, in order of preference:
   - `tmux capture-pane -t <tmux>:0 -p | tail -40` — shows the worker's current screen, including its last action and whether it's at the input prompt (idle) or thinking. Use this to tell when a worker is done.
   - JSONL transcripts under `~/.claude/projects/<sanitised-cwd>/<sid>.jsonl` — full conversation history, useful when you need detail.
   Avoid `until [ -f X ]; do sleep …; done` shell loops on file existence — they hide a stuck worker behind a 5-minute timeout.

4. **Merge edits.** When a worktree worker finishes:
   `{bin} merge-worktree --task {task_id} --worktree NAME`
   On success, the branch is in the project's main branch and the worktree dir stays for inspection. Four failure modes, each with its own recipe:
   - **Content conflict** (`ok=false`, no `blocked_by_dirty_tree`): git started the merge and hit overlapping committed history. Resolve manually in the worktree, or spawn a follow-up worker. Do NOT use `git stash` to dodge it.
   - **Blocked by dirty tree** (`ok=false`, `blocked_by_dirty_tree=true`, `overlap=[…]`): the user has uncommitted edits on the target branch in files the worker also touched. The merge was refused before touching the tree — nothing to clean up. Surface this verbatim via `task report --note \"merge blocked: <files>; commit/stash/revert and rerun merge-worktree\"` and stop. Do NOT auto-stash, do NOT touch the user's working tree, do NOT report status=done. The worker's branch is intact; the user merges once they clean those paths.
   - **Blocked by active orchestrator** (`ok=false`, `blocked_by_active_orchestrator=true`, `blocking_task=<other>`, `overlap=[…]`): another orchestrator currently holds an active reservation on overlapping files. Wait for them to release (their `task done`/`failed` clears their reservations), then rerun `merge-worktree`. Don't try to win the race by force — the other task is mid-edit and your merge would land on shifting ground.
   - **Hard error** (CLI exits with a non-JSON error): treat as recoverable; retry once, then report failed.
   After a successful merge:
   - **Build/test** the project (e.g. `cargo build`, `pnpm test`) — a green merge that breaks compilation is still a failure.
   - **Run `/simplify`** via the Skill tool to clean up the just-merged code; it may add follow-up commits on the main branch.
   - **Run `/bump`** afterwards to cut a version commit reflecting the final tree. If either step modified files, re-run build/test before proceeding.
   - **Free the reservation** so other tasks can proceed: `{bin} reservations release --task {task_id} --worker-id <tmux>`. (At task done/failed, all of this task's reservations are released automatically — but releasing per-worker as soon as a merge lands shortens the window other orchestrators wait.)

5. **Report progress** after each meaningful step (worker spawned, worker finished, merge attempted, plan changed):
   `{bin} task report --task {task_id} --status running --note \"<one line>\"`
   These notes surface in the user's Projects view. Keep them terse — milestones, not play-by-play.

6. **Track a checklist** (optional but strongly recommended for any task with 3+ logical steps). The active task card shows `done/total ✓` so the user can see progress against a plan, not just a heartbeat. Set the list once with a heredoc (one item per line, blank lines skipped); mark items as you finish them by 0-based index:
   `{bin} task todos set --task {task_id} --items \"$(cat <<'EOF'
plan worktree split
spawn worker A
spawn worker B
merge & verify
EOF
)\"`
   `{bin} task todos check --task {task_id} --index 1` — mark item done.
   `{bin} task todos uncheck --task {task_id} --index 1` — undo.
   `{bin} task todos clear --task {task_id}` — empty the list.
   Don't pre-list every micro-step; aim for a checklist the user could read in one breath.

7. **Finish.** When the user's task is complete, gather proof of work (see next section) and then:
   `{bin} task report --task {task_id} --status done --note \"<one line>\" --summary \"<multi-line briefing>\"`
   On unrecoverable failure: `--status failed --note \"<why>\"`.
   `done` lands the task in **Review** — the human (or a future agentic reviewer) signs off via the Projects UI before it transitions to actually Done. Your tmux session stays alive through Review so the user can ask follow-up questions; it's torn down on approval.

# Proof of work

Done without proof is not done. The user reviews finished tasks via **progressive disclosure**: they read a one-line proof + a single lead artifact first, scan supporting evidence next, and reach for the multi-line summary only if they want to dig deeper. Shape your final report to match — lead with the proof, not the briefing.

Three primitives:
- `{bin} task artifact add --task {task_id} --path PATH [--kind KIND] [--caption TEXT] [--lead]` — attach a file (copied into cc-hub's store, survives worktree cleanup) or a URL (stored as-is). `KIND` is free-form; common values: `screenshot`, `video`, `log`, `build`, `test`, `diff`, `file`, `url`. URL-shaped paths default to `kind=url`. Pass `--lead` on exactly one artifact — the strongest single piece of proof. Re-passing `--lead` on a later add moves the designation to the new artifact, so add the lead whenever you're sure which one it is.
- `{bin} task artifact list --task {task_id}` — review what's already attached, including which one is the current lead.

What counts as proof, and which to lead, by change type:
- **Web / UI** — screenshot or short screen recording of the rendered feature. Lead with the screenshot/recording. For regression fixes, attach before *and* after; lead the after.
- **CLI / library / backend** — terminal recording (asciinema if available) or captured command output (`--kind log`). Lead the recording, or the log if no recording is feasible.
- **Tests / CI / build** — the build log file, or a URL to the CI run (`--kind url`). Lead the green run.
- **Refactors (no behavioural change)** — a `diff` artifact summarising the change plus a `log` showing build + tests still pass. Lead the log (it's the \"still works\" proof).
- **Bug fixes** — a log showing the repro failing before and passing after the fix, OR a regression test added in the same change (mention its path in the summary). Lead the after-log, or the new test file.

On the final `task report --status done` call:
- `--note \"<headline proof>\"` — one line, present tense, declares the outcome and points at the lead. Examples: \"login redirects to /home; see screenshot\", \"import streams 2 GB CSV at 50 MB/s; see asciinema\". **Not** \"completed login flow\", \"task done\", or any status-update phrasing — the note is the proof.
- `--summary` — **optional appendix**, kept short. Cover only what the note + lead can't: key files changed (a few — not exhaustive) and what was deliberately out of scope. Don't restate the work — the user already read the note. Skip `--summary` entirely if there's nothing the lead artifact and note don't already convey.

Use a heredoc so summary newlines survive the shell:
`{bin} task report --task {task_id} --status done --note \"<headline proof>\" --summary \"$(cat <<'EOF'
<short appendix — files changed, out-of-scope>
EOF
)\"`

# Queuing follow-up work

If during this task you identify substantive follow-up work — a separate problem the user will likely want addressed but that's out of scope here — create a Backlog task for it instead of expanding scope or losing the idea:

`{bin} task create --backlog --prompt \"<scoped prompt for the follow-up>\" [--project-id ID]`

This writes a new task with status `backlog` and does NOT spawn an orchestrator. The user reviews backlog tasks from the Projects view and starts them manually when ready. Use it for: research findings that suggest a separate cleanup, bugs noticed but not in scope, refactors a worker recommended, etc. Keep the prompt self-contained — the future orchestrator won't have your context.

# Rules

- Don't ask the user clarifying questions. If the task is ambiguous, pick the most reasonable interpretation and note your assumption in the first status report.
- Don't implement yourself unless the task is genuinely tiny (a few lines in one file) and faster to do than to dispatch.
- Each worktree owns its files. Don't run two parallel worktree workers whose files overlap.
- If a worker creates or edits `.gitignore`, instruct it to include `.cc-hub-wt/` so cc-hub's worktree dirs don't pollute future commits.
- The user can micro-manage from the Sessions view; you don't need to surface every detail in reports — just milestones.

# Your task

{prompt}

Begin by writing your decomposition plan as the first `{bin} task report` call, then start dispatching.",
        task_id = task_id,
        project_id = project_id,
        root = project_root.display(),
        bin = bin,
        prompt = prompt,
    )
}

/// Create + persist a fresh task and spawn its orchestrator session.
///
/// Returns the `(TaskState, tmux_session_name)` so callers can immediately
/// queue the orchestrator prompt for dispatch (typically via the pending-
/// dispatch path: spawn now, send when Idle). The system prompt is built
/// against `cc_hub_bin`, which the caller resolves with
/// [`std::env::current_exe`].
///
/// Concretely:
/// 1. registers the project (if new) in `~/.cc-hub/projects.toml`
/// 2. writes the initial task state
/// 3. spawns `cc-hub-new` via the existing detached-tmux pathway
/// 4. records the resulting tmux name back on the state
///
/// This mirrors what `cc-hub orchestrate start` does, minus the synchronous
/// idle-poll/dispatch — the TUI prefers async dispatch so the keystroke
/// returns instantly.
pub fn spawn_orchestrator_for_new_task(
    project_root: &Path,
    project_name: &str,
    user_prompt: String,
) -> io::Result<(TaskState, String, String)> {
    let project_id = ensure_project_registered(project_root, project_name)?;
    let canonical_root = fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let mut state = TaskState::new(project_id, canonical_root.clone(), user_prompt);
    write_task_state(&state)?;

    let cwd = canonical_root.to_string_lossy().into_owned();
    let tmux_name = crate::spawn::spawn_claude_session(&cwd, None)?;

    state.orchestrator_tmux = Some(tmux_name.clone());
    state.touch();
    write_task_state(&state)?;

    let cc_hub_bin = std::env::current_exe()
        .map_err(|e| io::Error::other(format!("resolve cc-hub binary path: {}", e)))?;
    let orchestrator_prompt = build_orchestrator_prompt(&state, &cc_hub_bin);

    Ok((state, tmux_name, orchestrator_prompt))
}

/// User-initiated transition from Backlog to Running. Mirrors
/// spawn_orchestrator_for_new_task but operates on an existing Backlog task
/// instead of creating a new one. Called from the TUI when the user hits the
/// start-task keybind, and from \ on the CLI.
pub fn start_backlog_task(
    project_id: &str,
    task_id: &str,
) -> io::Result<(TaskState, String, String)> {
    let mut state = read_task_state(project_id, task_id)?;
    if state.status != TaskStatus::Backlog {
        return Err(io::Error::other(format!(
            "task is not in backlog (status = {:?})",
            state.status
        )));
    }
    state.status = TaskStatus::Running;
    state.touch();
    write_task_state(&state)?;

    let cwd = state.project_root.to_string_lossy().into_owned();
    let tmux_name = crate::spawn::spawn_claude_session(&cwd, None)?;

    state.orchestrator_tmux = Some(tmux_name.clone());
    state.touch();
    write_task_state(&state)?;

    let cc_hub_bin = std::env::current_exe()
        .map_err(|e| io::Error::other(format!("resolve cc-hub binary path: {}", e)))?;
    let orchestrator_prompt = build_orchestrator_prompt(&state, &cc_hub_bin);

    Ok((state, tmux_name, orchestrator_prompt))
}

/// Standard worktree path for `<task>-<name>` under `<root>/.cc-hub-wt/`.
pub fn worktree_path(project_root: &Path, task_id: &str, name: &str) -> PathBuf {
    project_root
        .join(".cc-hub-wt")
        .join(format!("{}-{}", task_id, name))
}

/// Branch name for a worktree. Mirrors the dir name so `git worktree list`
/// is readable.
pub fn worktree_branch(task_id: &str, name: &str) -> String {
    format!("cc-hub/{}-{}", task_id, name)
}

/// Detect the project's primary branch. Tries `origin/HEAD`, then `main`,
/// then `master`, falling back to `"main"` (which lets the caller's git
/// command surface the real failure rather than us inventing one).
pub fn detect_main_branch(root: &Path) -> String {
    if let Ok(out) = run_git(root, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]) {
        if out.status_ok {
            if let Some(name) = out.stdout.trim().strip_prefix("origin/") {
                return name.to_string();
            }
        }
    }
    for candidate in ["main", "master"] {
        let exists = run_git(root, &["rev-parse", "--verify", "--quiet", candidate])
            .map(|o| o.status_ok)
            .unwrap_or(false);
        if exists {
            return candidate.to_string();
        }
    }
    "main".to_string()
}

/// Result of running `git -C <root> <args>`. Distinguishes "non-zero exit"
/// (error in the command) from "couldn't even invoke git" (env problem).
pub struct GitOutput {
    pub status_ok: bool,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_git(root: &Path, args: &[&str]) -> io::Result<GitOutput> {
    let out = Command::new("git").arg("-C").arg(root).args(args).output()?;
    Ok(GitOutput {
        status_ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Create a worktree at `<root>/.cc-hub-wt/<task>-<name>` on a fresh branch
/// `cc-hub/<task>-<name>` based on `<base>`. Idempotent: a pre-existing
/// worktree is detected from git's stderr and reused so a re-running
/// orchestrator doesn't trip on its own previous attempt.
pub fn create_worktree(
    project_root: &Path,
    task_id: &str,
    name: &str,
    base_branch: &str,
) -> io::Result<PathBuf> {
    let path = worktree_path(project_root, task_id, name);
    let branch = worktree_branch(task_id, name);
    let out = run_git(
        project_root,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            &path.to_string_lossy(),
            base_branch,
        ],
    )?;
    if out.status_ok {
        return Ok(path);
    }
    // git's "already exists" / "is already checked out" messages mean a
    // previous run left this worktree behind; reuse rather than fail.
    let stderr = out.stderr.trim();
    let already = stderr.contains("already exists") || stderr.contains("already checked out");
    if already {
        log::info!("create_worktree: {} already exists, reusing", path.display());
        return Ok(path);
    }
    Err(io::Error::other(format!(
        "git worktree add failed: {}",
        stderr
    )))
}

/// Files modified-but-uncommitted in the working tree, from `git status
/// --porcelain -z`. Repo-relative paths, sorted, deduped. Used by the
/// merge pre-flight to detect whether an in-flight merge would clobber
/// the user's local edits.
pub fn dirty_paths(project_root: &Path) -> io::Result<Vec<String>> {
    let out = run_git(project_root, &["status", "--porcelain", "-z"])?;
    if !out.status_ok {
        return Err(io::Error::other(format!(
            "git status failed: {}",
            out.stderr.trim()
        )));
    }
    // -z output: NUL-terminated entries, each starting with two status
    // chars + space, then the path. Renames / copies emit an additional
    // NUL-separated source path with no leading status code; we keep both
    // sides so an overlap on either blocks the merge.
    let mut paths = Vec::new();
    let mut iter = out.stdout.split('\0').filter(|s| !s.is_empty()).peekable();
    while let Some(entry) = iter.next() {
        if entry.len() < 3 {
            continue;
        }
        let code = entry.as_bytes()[0];
        let path = entry[3..].to_string();
        paths.push(path);
        if matches!(code, b'R' | b'C') {
            if let Some(src) = iter.next() {
                paths.push(src.to_string());
            }
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Files changed by `feature_branch` relative to `main_branch`, from
/// `git diff <main>...<feature> --name-only -z` (three-dot — the merge
/// base, matching what git would actually pull in).
pub fn branch_changed_paths(
    project_root: &Path,
    main_branch: &str,
    feature_branch: &str,
) -> io::Result<Vec<String>> {
    let range = format!("{}...{}", main_branch, feature_branch);
    let out = run_git(project_root, &["diff", "--name-only", "-z", &range])?;
    if !out.status_ok {
        return Err(io::Error::other(format!(
            "git diff {} failed: {}",
            range,
            out.stderr.trim()
        )));
    }
    Ok(out
        .stdout
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

/// Merge `<branch>` into `<main>` from the project root. Performs two
/// pre-flight checks before any tree mutation:
///
/// 1. Consults the project's reservations file. If another task holds an
///    `active` reservation overlapping the branch's changed paths, returns
///    [`MergeOutcome::BlockedByActiveOrchestrator`]. `current_task_id` is
///    excluded so a task never blocks on itself. Reservations are
///    advisory — if the side file is unreadable we degrade silently
///    rather than blocking a merge that would otherwise succeed.
///
/// 2. If any uncommitted working-tree change overlaps a file the feature
///    branch also modified, returns [`MergeOutcome::BlockedByDirtyTree`].
///    `overlap` lists the repo-relative paths the user must commit,
///    stash, or revert before retrying.
///
/// Returns [`MergeOutcome::Conflict`] for the classical content-conflict
/// case where git started the merge and hit overlapping edits in
/// committed history.
///
/// Why we don't auto-stash anymore: an earlier version stashed before
/// merge and popped after, but a popping conflict left raw conflict
/// markers in source files, broke the build, and shifted resolution onto
/// the user without warning. Refusing up front is safer; the user's
/// recipe is one git command (`git stash`, `git commit`, or
/// `git checkout --`) followed by re-running `merge-worktree`.
pub fn merge_branch(
    project_root: &Path,
    main_branch: &str,
    feature_branch: &str,
    project_id: &str,
    current_task_id: &str,
) -> io::Result<(MergeOutcome, String, String)> {
    // Compute branch's changed-paths once for both preflight checks.
    let changed = branch_changed_paths(project_root, main_branch, feature_branch)?;

    // Preflight 1: cross-orchestrator reservation check. Done before we
    // mutate the working tree (`git checkout main`) so a merge that
    // can't proceed leaves no trace.
    if !changed.is_empty() {
        match crate::reservations::overlapping_active(
            project_id,
            current_task_id,
            &changed,
        ) {
            Ok(blockers) => {
                if let Some((blocking_task, paths)) = blockers.into_iter().next() {
                    return Ok((
                        MergeOutcome::BlockedByActiveOrchestrator {
                            task_id: blocking_task,
                            paths,
                        },
                        String::new(),
                        String::new(),
                    ));
                }
            }
            Err(e) => {
                // Reservations are advisory — degrade to current behaviour
                // rather than blocking a merge on an unreadable side file.
                log::warn!(
                    "merge_branch: reservations check failed (degrading to no-check): {}",
                    e
                );
            }
        }
    }

    // Preflight 2: refuse if dirty tree overlaps the branch's file set.
    // BTreeSet so the overlap list is stable for tests.
    let dirty: std::collections::BTreeSet<String> =
        dirty_paths(project_root)?.into_iter().collect();
    if !dirty.is_empty() {
        let branch_files: std::collections::BTreeSet<String> =
            changed.iter().cloned().collect();
        let overlap: Vec<String> = dirty.intersection(&branch_files).cloned().collect();
        if !overlap.is_empty() {
            return Ok((
                MergeOutcome::BlockedByDirtyTree { overlap },
                String::new(),
                String::new(),
            ));
        }
        // Dirty in non-overlapping files only — git carries those changes
        // through the checkout and merge cleanly. No stash needed.
    }

    let checkout = run_git(project_root, &["checkout", main_branch])?;
    if !checkout.status_ok {
        return Err(io::Error::other(format!(
            "git checkout {} failed: {}",
            main_branch,
            checkout.stderr.trim()
        )));
    }
    let msg = format!("cc-hub: merge {} into {}", feature_branch, main_branch);
    let out = run_git(
        project_root,
        &["merge", "--no-ff", "-m", &msg, feature_branch],
    )?;
    let outcome = if out.status_ok {
        MergeOutcome::Ok
    } else {
        let detail = if !out.stderr.trim().is_empty() {
            out.stderr.clone()
        } else {
            out.stdout.clone()
        };
        MergeOutcome::Conflict {
            detail: detail.trim().to_string(),
        }
    };
    Ok((outcome, out.stdout, out.stderr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Tests that mutate `$HOME` to redirect `cc_hub_home()` at a tempdir
    // would otherwise race each other (HOME is process-global). Hold this
    // mutex for the duration of any such test.
    static HOME_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn project_id_is_stable_and_sanitised() {
        let a = project_id_for_path(Path::new("/home/j-light/git/self/cc-hub"));
        assert_eq!(a, "home-j-light-git-self-cc-hub");

        // collapses runs of separators
        let b = project_id_for_path(Path::new("/foo//bar/_baz_"));
        assert_eq!(b, "foo-bar-baz");

        // empty-ish input falls back
        let c = project_id_for_path(Path::new("/"));
        assert_eq!(c, "root");
    }

    #[test]
    fn task_state_round_trips_through_serde() {
        let mut s = TaskState::new(
            "myproj".into(),
            PathBuf::from("/tmp/myproj"),
            "do the thing".into(),
        );
        s.note = Some("kicked off worker A".into());
        s.workers.push(Worker {
            tmux_name: "cchub-1".into(),
            cwd: PathBuf::from("/tmp/myproj"),
            worktree: Some("a".into()),
            readonly: false,
            spawned_at: 42,
        });
        s.merges.push(MergeRecord {
            worktree: "a".into(),
            at: 99,
            outcome: MergeOutcome::Conflict {
                detail: "conflict in foo.rs".into(),
            },
        });

        let body = serde_json::to_string(&s).unwrap();
        let back: TaskState = serde_json::from_str(&body).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn artifact_round_trips_through_serde() {
        let a = Artifact {
            kind: "screenshot".into(),
            path: "/abs/path/123-foo.png".into(),
            original: "./foo.png".into(),
            caption: Some("login screen after fix".into()),
            added_at: 1_700_000_000,
        };
        let body = serde_json::to_string(&a).unwrap();
        let back: Artifact = serde_json::from_str(&body).unwrap();
        assert_eq!(back, a);

        // No-caption variant — Option<String>::None must serialise+deserialise
        // without surprising the rest of the schema.
        let b = Artifact {
            kind: "url".into(),
            path: "https://example.com/build/42".into(),
            original: "https://example.com/build/42".into(),
            caption: None,
            added_at: 1_700_000_001,
        };
        let body = serde_json::to_string(&b).unwrap();
        let back: Artifact = serde_json::from_str(&body).unwrap();
        assert_eq!(back, b);
    }

    #[test]
    fn task_state_with_artifacts_and_summary_round_trips() {
        let mut s = TaskState::new(
            "myproj".into(),
            PathBuf::from("/tmp/myproj"),
            "do the thing".into(),
        );
        s.summary = Some("shipped feature X.\n\nverified Y, Z.".into());
        s.artifacts.push(Artifact {
            kind: "screenshot".into(),
            path: "/store/123-shot.png".into(),
            original: "shot.png".into(),
            caption: Some("after".into()),
            added_at: 7,
        });
        s.artifacts.push(Artifact {
            kind: "url".into(),
            path: "https://ci.example/run/9".into(),
            original: "https://ci.example/run/9".into(),
            caption: None,
            added_at: 8,
        });
        s.lead_artifact = Some(0);

        let body = serde_json::to_string(&s).unwrap();
        let back: TaskState = serde_json::from_str(&body).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn task_state_back_compat_when_artifacts_and_summary_missing() {
        // Mirrors a state.json written by an older cc-hub: no `summary`
        // key, no `artifacts` key. Must still parse, with both fields
        // defaulting empty.
        let raw = r#"{
            "task_id": "t-1",
            "project_id": "p",
            "project_root": "/tmp/p",
            "status": "running",
            "prompt": "hi",
            "created_at": 1,
            "updated_at": 2
        }"#;
        let s: TaskState = serde_json::from_str(raw).unwrap();
        assert_eq!(s.summary, None);
        assert!(s.artifacts.is_empty());
        assert_eq!(s.lead_artifact, None);
        // `title` is also serde(default) for back-compat with state.json
        // written before the Haiku task-title feature landed.
        assert_eq!(s.title, None);
    }

    #[test]
    fn set_task_title_persists_through_round_trip() {
        // `set_task_title` writes through `cc_hub_home()` which is a
        // thin wrapper around `dirs::home_dir()` (i.e. `$HOME`). Point it
        // at a tempdir so the test never touches the real user state.
        let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        // Seed a state.json without a title set — the typical post-creation
        // shape before the background titler has run.
        let project_id = "round-trip-proj".to_string();
        let task_id_set;
        {
            let initial = TaskState::new(
                project_id.clone(),
                PathBuf::from("/tmp/proj"),
                "build the thing".into(),
            );
            task_id_set = initial.task_id.clone();
            write_task_state(&initial).expect("write seed state");
        }

        let result = set_task_title(&project_id, &task_id_set, "build thing")
            .expect("set_task_title");
        assert_eq!(result.title.as_deref(), Some("build thing"));

        let loaded = read_task_state(&project_id, &task_id_set)
            .expect("read state back from disk");
        assert_eq!(loaded.title.as_deref(), Some("build thing"));
        assert!(loaded.updated_at >= loaded.created_at, "touch() should bump updated_at");

        // Restore HOME to keep other tests in the process oblivious.
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn task_status_serialises_lowercase() {
        let s = serde_json::to_string(&TaskStatus::Running).unwrap();
        assert_eq!(s, "\"running\"");
        let f = serde_json::to_string(&TaskStatus::Failed).unwrap();
        assert_eq!(f, "\"failed\"");
    }

    #[test]
    fn task_status_backlog_serialises_lowercase() {
        assert_eq!(
            serde_json::to_string(&TaskStatus::Backlog).unwrap(),
            "\"backlog\""
        );
    }

    #[test]
    fn backlog_task_round_trips_through_serde() {
        let s = TaskState::new_backlog(
            "myproj".into(),
            PathBuf::from("/tmp/myproj"),
            "queued for later".into(),
        );
        let body = serde_json::to_string(&s).unwrap();
        let back: TaskState = serde_json::from_str(&body).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.status, TaskStatus::Backlog);
    }

    #[test]
    fn worktree_path_includes_task_id() {
        let p = worktree_path(Path::new("/repo"), "t-123", "edit");
        assert_eq!(p, PathBuf::from("/repo/.cc-hub-wt/t-123-edit"));
    }

    #[test]
    fn task_id_format() {
        let id = new_task_id();
        assert!(id.starts_with("t-"));
        assert!(id.len() > 4);
    }

    #[test]
    fn orchestrator_prompt_substitutes_ids_and_user_prompt() {
        let state = TaskState::new(
            "myproj-42".into(),
            PathBuf::from("/work/myproj"),
            "redo the import pipeline so it streams".into(),
        );
        let bin = Path::new("/opt/cc-hub/bin/cc-hub");
        let p = build_orchestrator_prompt(&state, bin);

        // Identity substitutions.
        assert!(p.contains(&state.task_id), "missing task_id");
        assert!(p.contains("myproj-42"), "missing project_id");
        assert!(p.contains("/work/myproj"), "missing project_root");
        assert!(
            p.contains("redo the import pipeline so it streams"),
            "user prompt missing"
        );

        // Binary path appears in every primitive.
        let bin_s = bin.display().to_string();
        let expected_primitives = [
            format!(
                "{} spawn-worker --task {} --readonly",
                bin_s, state.task_id
            ),
            format!(
                "{} spawn-worker --task {} --worktree",
                bin_s, state.task_id
            ),
            format!(
                "{} merge-worktree --task {} --worktree",
                bin_s, state.task_id
            ),
            format!("{} task report --task {}", bin_s, state.task_id),
        ];
        for cmd in &expected_primitives {
            assert!(p.contains(cmd), "primitive missing from prompt: {}", cmd);
        }

        // Load-bearing rules — keep these concise checks so wording can drift.
        assert!(p.contains("decompose"), "missing decomposition framing");
        assert!(
            p.contains("clarifying"),
            "missing 'don't ask clarifying questions' rule"
        );
        // Worktree-gitignore guidance — caught the one shortcoming from
        // the first end-to-end run.
        assert!(
            p.contains(".cc-hub-wt/"),
            "missing .cc-hub-wt/ gitignore guidance"
        );
        // Monitor channel guidance — replaces the 5-min `until` loop
        // pattern that the orchestrator picked up first time.
        assert!(
            p.contains("tmux capture-pane"),
            "missing capture-pane monitor guidance"
        );

        // Proof-of-work guidance — done isn't done without evidence.
        assert!(
            p.contains("Proof of work"),
            "missing proof-of-work section header"
        );
        assert!(
            p.contains(&format!(
                "{} task artifact add --task {}",
                bin_s, state.task_id
            )),
            "missing artifact-add primitive in proof-of-work section"
        );
        assert!(
            p.contains("--summary"),
            "missing --summary guidance for final report"
        );
        // Progressive-disclosure framing — the note is the proof, not a
        // status update; exactly one artifact must be flagged --lead.
        assert!(
            p.contains("--lead"),
            "missing --lead guidance in proof-of-work section"
        );
        assert!(
            p.contains("headline proof"),
            "missing headline-proof framing for --note"
        );

        // Reservations lifecycle — declare/upgrade/list/release must all be
        // referenced so the orchestrator coordinates with peers per the
        // soft-reservations design.
        for verb in ["declare", "upgrade", "list", "release"] {
            assert!(
                p.contains(&format!("reservations {}", verb)),
                "missing `reservations {}` reference in prompt",
                verb
            );
        }
        assert!(
            p.contains("research while waiting"),
            "missing 'research while waiting' framing — orchestrator should \
             front-load read-only subtasks while yielded, not block idle"
        );
        assert!(
            p.contains("blocked_by_active_orchestrator"),
            "missing fourth merge-failure recipe (blocked by active orchestrator)"
        );
        // The recipe must reference the actual JSON fields the merge CLI
        // emits (cli.rs:455-465: blocked_by_active_orchestrator + blocking_task
        // + overlap), otherwise an orchestrator following the recipe will
        // grep for a non-existent `task_id` field and silently fall through.
        assert!(
            p.contains("blocking_task"),
            "fourth merge-failure recipe must name the `blocking_task` JSON \
             field emitted by the merge-worktree CLI, not a different one"
        );
        // Post-merge automation: each completed task should land on a green,
        // simplified, version-stamped main.
        for skill in ["/simplify", "/bump"] {
            assert!(
                p.contains(skill),
                "missing post-merge `{}` step in prompt",
                skill
            );
        }
    }

    #[test]
    fn remove_project_deletes_registry_entry_and_state_dir() {
        let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let project_root = home.path().join("proj-under-test");
        fs::create_dir_all(&project_root).expect("mkdir project root");
        let project_id =
            ensure_project_registered(&project_root, "proj").expect("register");

        // Seed a task so the project state dir actually exists on disk.
        let mut state = TaskState::new(
            project_id.clone(),
            project_root.clone(),
            "do thing".into(),
        );
        state.orchestrator_tmux = None;
        write_task_state(&state).expect("write seed task state");

        let proj_dir = project_state_dir(&project_id).expect("project_state_dir");
        assert!(proj_dir.exists(), "project state dir should exist after seed");

        remove_project(&project_id).expect("remove_project");

        let after = load_projects();
        assert!(
            !after.projects.iter().any(|p| p.id == project_id),
            "project should be gone from registry"
        );
        assert!(
            !proj_dir.exists(),
            "project state dir should have been removed"
        );

        // Idempotent: a second call against an already-removed id is Ok.
        remove_project(&project_id).expect("idempotent remove");

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}
