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
//! that mutate it live in `bin/src/cli/`; the TUI consumer lives in
//! `lib/src/ui/`.
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

use crate::agent::AgentKind;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

mod gc;
mod git;
mod prompts;

pub use gc::{gc_worktrees, scan_worktrees, GcOutcome, WorktreeEntry};
pub use git::{
    branch_changed_paths, create_worktree, detect_main_branch, dirty_paths, is_valid_worktree_name,
    merge_branch, run_git, worktree_branch, worktree_path, GitOutput,
};
pub use prompts::{
    build_orchestrator_prompt, build_review_approval_prompt, orchestrator_prompt_prefix,
    resolve_cc_hub_bin, restart_task, spawn_orchestrator_for_new_task, start_backlog_task,
};

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

/// Root of the personal-board task store: `~/.cc-hub/tasks/<task-id>/`.
/// Same per-task layout (`state.json` + `state.lock`) as the orchestrated
/// store, so both flavors share the lock + atomic-write machinery.
pub fn personal_tasks_dir() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("tasks"))
}

pub fn personal_task_dir(task_id: &str) -> Option<PathBuf> {
    personal_tasks_dir().map(|d| d.join(task_id))
}

/// Task directory for either flavor: `Some(pid)` routes to the project
/// store, `None` to the personal store.
pub fn task_dir_for(project_id: Option<&str>, task_id: &str) -> Option<PathBuf> {
    match project_id {
        Some(pid) => task_state_dir(pid, task_id),
        None => personal_task_dir(task_id),
    }
}

pub fn task_state_file(project_id: &str, task_id: &str) -> Option<PathBuf> {
    task_state_dir(project_id, task_id).map(|d| d.join("state.json"))
}

pub fn task_state_file_for(project_id: Option<&str>, task_id: &str) -> Option<PathBuf> {
    task_dir_for(project_id, task_id).map(|d| d.join("state.json"))
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

/// Personal-board task id: same nanos scheme, `tk-` prefix (the prefix the
/// board used before unification, kept so board-born tasks stay recognizable
/// and migrated ids remain valid).
pub fn new_personal_task_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("tk-{}", nanos)
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

fn default_claude_agent_id() -> String {
    "claude".into()
}

fn default_claude_agent_kind() -> AgentKind {
    AgentKind::Claude
}

/// The one status enum for both task flavors. Orchestrated tasks flow
/// Backlog → Running → Review → Merging → Done; personal-board tasks flow
/// Backlog ("To-Do") → Planning → Running ("In Progress") → Done and never
/// touch Review/Merging. Wire names of the orchestrated states are unchanged
/// from before unification, so existing `state.json` files and the CLI
/// `--status` flag are unaffected; `planning` is a new accepted value that
/// orchestrated flows never produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Backlog,
    /// Personal-board only: an agent was assigned and told to present a plan
    /// first; the card waits for the user to approve it (Space → Running).
    Planning,
    Running,
    /// Orchestrator finished its work and the PR is open, waiting on a human
    /// (or future agentic reviewer) to approve or request changes via the
    /// Projects UI. The orchestrator's tmux stays alive through Review so
    /// follow-up "request changes" rounds can iterate on the same worktree.
    Review,
    /// PR was approved; the orchestrator is now actively merging the feature
    /// branch into main. Only one task per project can be in `Merging` at
    /// once — the project-level merge lock enforces serialization. The
    /// transition Merging → Done happens when `cc-hub pr merge` finishes
    /// (lock released, /simplify + /bump done).
    Merging,
    Done,
}

impl TaskStatus {
    /// Lowercase wire/CLI name. Must match `#[serde(rename_all = "lowercase")]`
    /// above so JSON round-trips and CLI `--status` agree.
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Backlog => "backlog",
            TaskStatus::Planning => "planning",
            TaskStatus::Running => "running",
            TaskStatus::Review => "review",
            TaskStatus::Merging => "merging",
            TaskStatus::Done => "done",
        }
    }
}

impl std::str::FromStr for TaskStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "backlog" => Ok(TaskStatus::Backlog),
            "planning" => Ok(TaskStatus::Planning),
            "running" => Ok(TaskStatus::Running),
            "review" => Ok(TaskStatus::Review),
            "merging" => Ok(TaskStatus::Merging),
            "done" => Ok(TaskStatus::Done),
            _ => Err(()),
        }
    }
}

/// Which flow a task belongs to, derived from `TaskState::project_id`:
/// `Personal` board tasks (`None`) admit the plan/reopen edges; `Orchestrated`
/// tasks (`Some`) keep the strict PR-pipeline edges (Done terminal, Merging
/// discipline).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Personal,
    Orchestrated,
}

/// Task priority, personal-board flavored (P1 highest … P4 lowest). Variants
/// are declared in ascending order (`P1 < P2 < P3 < P4`) so a plain ascending
/// sort puts the most urgent first. Lives on the unified [`TaskState`];
/// orchestrated flows ignore it, and `P3` (the default) is skipped during
/// serialization so orchestrated `state.json` files gain no key. The
/// `snake_case` wire form ("p1".."p4") is unchanged from the pre-unification
/// personal board, so migrated `tasks.json` values parse as-is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    P1,
    P2,
    #[default]
    P3,
    P4,
}

impl TaskPriority {
    /// True for the priority new tasks get; used by `skip_serializing_if`.
    pub fn is_default(&self) -> bool {
        *self == TaskPriority::default()
    }

    /// Short badge label shown on the card (`P1`–`P4`).
    pub fn label(self) -> &'static str {
        match self {
            TaskPriority::P1 => "P1",
            TaskPriority::P2 => "P2",
            TaskPriority::P3 => "P3",
            TaskPriority::P4 => "P4",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worker {
    #[serde(default = "default_claude_agent_id")]
    pub agent_id: String,
    #[serde(default = "default_claude_agent_kind")]
    pub agent_kind: AgentKind,
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
    Conflict {
        detail: String,
    },
    /// Pre-flight refused: the working tree on the target branch has
    /// uncommitted edits in files the feature branch also modified, so
    /// the merge would either fail with "would be overwritten" or — worse
    /// — be auto-stashed and produce conflict markers on pop. We detect
    /// this up front and decline to touch the tree. `overlap` lists the
    /// repo-relative paths the user must commit, stash, or revert before
    /// retrying. Distinct from `Conflict`, which means git started the
    /// merge and hit content conflicts during it.
    BlockedByDirtyTree {
        overlap: Vec<String>,
    },
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
    /// `None` marks a personal-board task (stored under `~/.cc-hub/tasks/`);
    /// `Some` an orchestrated one (under `~/.cc-hub/projects/<pid>/tasks/`).
    /// Existing orchestrated `state.json` files carry both fields, so `Some`
    /// deserializes and re-serializes identically to the pre-Option schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<PathBuf>,
    // ── personal-board fields; all defaulted + skipped-when-default so
    //    orchestrated state.json files and `task show --json` gain no keys.
    /// When the task landed in Done, for the board's Done column stamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<i64>,
    #[serde(default, skip_serializing_if = "TaskPriority::is_default")]
    pub priority: TaskPriority,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Where the board-assigned agent runs (personal flow only; the
    /// orchestrated flow derives cwd from `project_root`/worktrees).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Agent id of the board assignment (e.g. "claude").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Mux session of the board-assigned agent, from spawn time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux: Option<String>,
    /// Agent session id, resolved from the first scan that sees the spawned
    /// tmux. Outlives the tmux session, so `f` can resume after it dies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Filled in by the orchestrator the first time it reports — cc-hub
    /// can't know its session id at spawn time.
    #[serde(default)]
    pub orchestrator_session_id: Option<String>,
    #[serde(default = "default_claude_agent_id")]
    pub orchestrator_agent_id: String,
    #[serde(default = "default_claude_agent_kind")]
    pub orchestrator_agent_kind: AgentKind,
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
    /// Unix timestamp of the last time the backlog triager considered this
    /// task. Bounds the rate of Claude calls per dormant task to one per
    /// `[backlog].ttl_secs`. `None` means never triaged.
    #[serde(default)]
    pub triaged_at: Option<i64>,
    /// Unix timestamp of the last time the auto-reviewer ran against this
    /// task's current Review round. Cleared whenever the task re-enters
    /// Review (orchestrator opens PR, or user/orchestrator flips it back
    /// after iteration), so each round gets exactly one auto-review pass.
    /// `None` means the current round hasn't been auto-reviewed yet.
    #[serde(default)]
    pub last_auto_reviewed_at: Option<i64>,
    /// Version of the project that was shipped as a result of this task,
    /// captured at the moment the orchestrator first declares completion
    /// (Running → Review/Done). Read from the project's manifest
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
            project_id: Some(project_id),
            project_root: Some(project_root),
            done_at: None,
            priority: TaskPriority::default(),
            tags: Vec::new(),
            cwd: None,
            agent_id: None,
            tmux: None,
            session_id: None,
            orchestrator_session_id: None,
            orchestrator_agent_id: default_claude_agent_id(),
            orchestrator_agent_kind: default_claude_agent_kind(),
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
            triaged_at: None,
            last_auto_reviewed_at: None,
            shipped_version: None,
        }
    }

    pub fn new_backlog(project_id: String, project_root: PathBuf, prompt: String) -> Self {
        let now = now_unix_secs();
        Self {
            task_id: new_task_id(),
            project_id: Some(project_id),
            project_root: Some(project_root),
            done_at: None,
            priority: TaskPriority::default(),
            tags: Vec::new(),
            cwd: None,
            agent_id: None,
            tmux: None,
            session_id: None,
            orchestrator_session_id: None,
            orchestrator_agent_id: default_claude_agent_id(),
            orchestrator_agent_kind: default_claude_agent_kind(),
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
            triaged_at: None,
            last_auto_reviewed_at: None,
            shipped_version: None,
        }
    }

    /// A personal-board task: no project, Backlog ("To-Do") start, board id
    /// prefix (`tk-`) so board-born and project-born tasks stay tellable
    /// apart in logs and on disk.
    pub fn new_personal(prompt: String) -> Self {
        let mut state = Self::new(String::new(), PathBuf::new(), prompt);
        state.task_id = new_personal_task_id();
        state.project_id = None;
        state.project_root = None;
        state.status = TaskStatus::Backlog;
        state
    }

    pub fn touch(&mut self) {
        self.updated_at = now_unix_secs();
    }

    /// Which legal-edge set this task follows; see [`TaskKind`].
    pub fn kind(&self) -> TaskKind {
        if self.project_id.is_some() {
            TaskKind::Orchestrated
        } else {
            TaskKind::Personal
        }
    }

    /// Project id + root, or `InvalidInput` when called on a personal task.
    /// Orchestrated entry points (ops, PR flow, triage, prompts) use this
    /// instead of unwrapping so a personal task routed into an
    /// orchestrated-only path fails with a message, not a panic.
    pub fn require_project(&self) -> io::Result<(&str, &Path)> {
        match (self.project_id.as_deref(), self.project_root.as_deref()) {
            (Some(id), Some(root)) => Ok((id, root)),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "task {} is a personal-board task (no project); this operation needs an \
                     orchestrated task",
                    self.task_id
                ),
            )),
        }
    }
}

/// Read a task state file; missing file returns NotFound, parse errors
/// surface as InvalidData so callers can distinguish "no such task" from
/// "schema drift".
pub fn read_task_state(project_id: &str, task_id: &str) -> io::Result<TaskState> {
    read_task_state_for(Some(project_id), task_id)
}

/// [`read_task_state`] for either flavor: `None` reads the personal store.
pub fn read_task_state_for(project_id: Option<&str>, task_id: &str) -> io::Result<TaskState> {
    let path =
        task_state_file_for(project_id, task_id).ok_or_else(|| io::Error::other("no home dir"))?;
    let raw = fs::read_to_string(&path)?;
    serde_json::from_str(&raw).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {}", path.display(), e),
        )
    })
}

/// Atomically write a task state file via tempfile + rename, routed by the
/// state's own flavor (`project_id` set → project store, unset → personal
/// store). Creates parent dirs on demand.
pub fn write_task_state(state: &TaskState) -> io::Result<()> {
    let path = task_state_file_for(state.project_id.as_deref(), &state.task_id)
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

/// Take the per-task exclusive advisory lock that serializes every
/// read-mutate-write over this task's `state.json` / `pr.json` across
/// processes (TUI, CLI verbs run from agent sessions, triage, auto-review).
/// The lock lives in a dedicated `state.lock` file next to `state.json`
/// because flock follows the inode — a tempfile+rename store can't be
/// locked directly. Returns `None` when the task directory doesn't exist
/// yet: there's nothing to protect, and the caller's read will surface
/// `NotFound` with its usual error.
pub(crate) fn lock_task_state(
    project_id: Option<&str>,
    task_id: &str,
) -> io::Result<Option<fs::File>> {
    use fs2::FileExt;
    let Some(dir) = task_dir_for(project_id, task_id) else {
        return Ok(None);
    };
    if !dir.exists() {
        return Ok(None);
    }
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("state.lock"))?;
    f.lock_exclusive()?;
    Ok(Some(f))
}

/// Single source of truth for legal task-status transitions, for BOTH task
/// flavors. Enforced centrally by [`update_task_state`] /
/// [`update_task_state_no_touch`], so no CLI verb, TUI keybind, or daemon can
/// invent an edge the kanban flow doesn't have. Per-verb guards (e.g.
/// "leaving Backlog requires an orchestrator spawn") add context-specific
/// rules on top.
///
/// Legal edges and the flows that produce them:
///
/// | from → to           | kind         | produced by                                         |
/// |---------------------|--------------|-----------------------------------------------------|
/// | Backlog → Running   | both         | `task start`, triage promotion; board manual move   |
/// | Running → Backlog   | both         | spawn-failure claim rollback; board manual move     |
/// | Backlog ↔ Planning  | personal     | board assign (`s`/`S`) / manual move back           |
/// | Running → Planning  | personal     | re-assign a stalled In-Progress card                |
/// | Done → Planning     | personal     | re-assign a finished card (reopen with agent)       |
/// | Planning → Running  | personal     | plan approved (Space), manual move                  |
/// | Running → Done      | both         | `pr close` while iterating; board finish            |
/// | Backlog → Done      | personal     | Space checks off a To-Do card directly              |
/// | Planning → Done     | personal     | finish an assigned card without approving the plan  |
/// | Done → Backlog      | personal     | board reopen (Space on a Done card)                 |
/// | Done → Running      | personal     | board manual move off Done                          |
/// | Running → Review    | orchestrated | `pr create`, `pr reopen`, `task report`             |
/// | Review  → Running   | orchestrated | `pr request-changes`                                |
/// | Review  → Merging   | orchestrated | approve (TUI `Space`, `pr merge`)                   |
/// | Review  → Done      | orchestrated | PR-less approve, `pr close`, explicit done          |
/// | Merging → Review    | orchestrated | merge-conflict demotion, dead-orchestrator rollback |
/// | Merging → Done      | orchestrated | `pr finalize`, `pr close`                           |
///
/// Self-transitions are always allowed (idempotent re-reports). For
/// orchestrated tasks Done stays terminal and Backlog is re-entered only by
/// the claim-first spawn rollback; the personal board additionally admits
/// the plan gate (Planning) and reopening finished cards.
pub fn validate_status_transition(
    from: &TaskStatus,
    to: &TaskStatus,
    kind: TaskKind,
) -> Result<(), String> {
    use TaskStatus::*;
    let both = from == to
        || matches!(
            (from, to),
            (Backlog, Running) | (Running, Backlog) | (Running, Done)
        );
    let legal = both
        || match kind {
            TaskKind::Personal => matches!(
                (from, to),
                (Backlog, Planning)
                    | (Planning, Backlog)
                    | (Planning, Running)
                    // Re-assigning a stalled or finished card spawns a fresh
                    // planning agent: any column can (re-)enter Planning.
                    | (Running, Planning)
                    | (Done, Planning)
                    // Space checks off a card regardless of phase: a To-Do
                    // that never started, or a Planning card whose agent the
                    // user abandoned.
                    | (Backlog, Done)
                    | (Planning, Done)
                    | (Done, Backlog)
                    | (Done, Running)
            ),
            TaskKind::Orchestrated => matches!(
                (from, to),
                (Running, Review)
                    | (Review, Running)
                    | (Review, Merging)
                    | (Review, Done)
                    | (Merging, Review)
                    | (Merging, Done)
            ),
        };
    if legal {
        Ok(())
    } else {
        let flow = match kind {
            TaskKind::Personal => {
                "personal flow is Backlog → Planning → Running → Done; Done can reopen to \
                 Backlog/Running"
            }
            TaskKind::Orchestrated => {
                "orchestrated flow is Backlog → Running → Review → Merging → Done; Review can \
                 bounce to Running/Merging, Merging back to Review; Done is terminal"
            }
        };
        Err(format!(
            "illegal task status transition {:?} → {:?} ({})",
            from, to, flow
        ))
    }
}

fn update_task_state_inner<F>(
    project_id: Option<&str>,
    task_id: &str,
    touch: bool,
    f: F,
) -> io::Result<TaskState>
where
    F: FnOnce(&mut TaskState),
{
    try_update_task_state_inner(project_id, task_id, touch, |s| {
        f(s);
        true
    })
    .map(|(state, _)| state)
}

/// Locked read-mutate-write like [`update_task_state`], except the closure
/// decides whether to persist: returning `false` aborts without writing (or
/// touching), so a guard that REJECTS a command inside the lock doesn't bump
/// `updated_at` — and reshuffle the kanban — as a side effect of refusing.
/// Returns the (possibly unwritten) state plus whether it was written.
pub fn try_update_task_state<F>(
    project_id: &str,
    task_id: &str,
    f: F,
) -> io::Result<(TaskState, bool)>
where
    F: FnOnce(&mut TaskState) -> bool,
{
    try_update_task_state_inner(Some(project_id), task_id, true, f)
}

fn try_update_task_state_inner<F>(
    project_id: Option<&str>,
    task_id: &str,
    touch: bool,
    f: F,
) -> io::Result<(TaskState, bool)>
where
    F: FnOnce(&mut TaskState) -> bool,
{
    let _lock = lock_task_state(project_id, task_id)?;
    let mut state = read_task_state_for(project_id, task_id)?;
    let prev_status = state.status;
    if !f(&mut state) {
        return Ok((state, false));
    }
    if state.status != prev_status {
        // The kind axis comes from the state itself: a personal task can't
        // gain orchestrated edges by being routed through this path.
        validate_status_transition(&prev_status, &state.status, state.kind())
            .map_err(|msg| io::Error::new(io::ErrorKind::InvalidInput, msg))?;
    }
    if touch {
        state.touch();
    }
    write_task_state(&state)?;
    Ok((state, true))
}

#[cfg(test)]
mod status_transition_tests {
    use super::*;

    #[test]
    fn orchestrated_legal_edges_pass() {
        use TaskStatus::*;
        for (from, to) in [
            (Backlog, Running),
            // Spawn-failure rollback re-enters Backlog from Running.
            (Running, Backlog),
            (Running, Review),
            (Running, Done),
            (Review, Running),
            (Review, Merging),
            (Review, Done),
            (Merging, Review),
            (Merging, Done),
            // Idempotent self-transitions.
            (Backlog, Backlog),
            (Done, Done),
        ] {
            assert!(
                validate_status_transition(&from, &to, TaskKind::Orchestrated).is_ok(),
                "orchestrated {:?} → {:?} should be legal",
                from,
                to
            );
        }
    }

    #[test]
    fn orchestrated_illegal_edges_fail() {
        use TaskStatus::*;
        for (from, to) in [
            (Backlog, Review),
            (Backlog, Merging),
            (Backlog, Done),
            (Running, Merging),
            (Review, Backlog),
            (Merging, Running),
            (Merging, Backlog),
            (Done, Running),
            (Done, Review),
            (Done, Merging),
            (Done, Backlog),
            // The personal plan gate never applies to orchestrated tasks.
            (Backlog, Planning),
            (Planning, Running),
        ] {
            assert!(
                validate_status_transition(&from, &to, TaskKind::Orchestrated).is_err(),
                "orchestrated {:?} → {:?} should be illegal",
                from,
                to
            );
        }
    }

    #[test]
    fn personal_legal_edges_pass() {
        use TaskStatus::*;
        for (from, to) in [
            // Shared edges.
            (Backlog, Running),
            (Running, Backlog),
            (Running, Done),
            (Backlog, Backlog),
            // The plan gate: assign → approve.
            (Backlog, Planning),
            (Planning, Backlog),
            (Planning, Running),
            // Re-assign flows re-enter Planning from anywhere.
            (Running, Planning),
            (Done, Planning),
            // Space checks off a card regardless of phase.
            (Backlog, Done),
            (Planning, Done),
            // Done reopens on the board.
            (Done, Backlog),
            (Done, Running),
        ] {
            assert!(
                validate_status_transition(&from, &to, TaskKind::Personal).is_ok(),
                "personal {:?} → {:?} should be legal",
                from,
                to
            );
        }
    }

    #[test]
    fn personal_illegal_edges_fail() {
        use TaskStatus::*;
        for (from, to) in [
            // The PR pipeline is orchestrated-only.
            (Running, Review),
            (Review, Merging),
            (Review, Done),
            (Merging, Done),
            (Backlog, Review),
            (Backlog, Merging),
            (Planning, Review),
        ] {
            assert!(
                validate_status_transition(&from, &to, TaskKind::Personal).is_err(),
                "personal {:?} → {:?} should be illegal",
                from,
                to
            );
        }
    }

    /// CLI-contract snapshot: an orchestrated `state.json` written BEFORE the
    /// task-model unification must round-trip through the unified struct with
    /// the exact same JSON key set — `task show --json` dumps this
    /// serialization verbatim, and live orchestrator agents parse it.
    #[test]
    fn pre_unification_state_roundtrips_with_identical_key_set() {
        let fixture = serde_json::json!({
            "task_id": "t-1750000000000000000",
            "project_id": "p-fixture",
            "project_root": "/tmp/p-fixture",
            "orchestrator_session_id": "sid-1",
            "orchestrator_agent_id": "claude",
            "orchestrator_agent_kind": "claude",
            "orchestrator_tmux": "cchub-orch-1",
            "status": "review",
            "prompt": "do the fixture thing",
            "created_at": 1750000000,
            "updated_at": 1750000100,
            "note": "PR #1: fixture",
            "summary": "did the thing",
            "title": "Fixture Thing",
            "workers": [],
            "merges": [],
            "artifacts": [],
            "todos": [],
            "lead_artifact": null,
            "triaged_at": null,
            "last_auto_reviewed_at": null,
            "shipped_version": "0.62.0"
        });
        let state: TaskState = serde_json::from_value(fixture.clone()).expect("parse fixture");
        let out = serde_json::to_value(&state).expect("serialize");

        let keys = |v: &serde_json::Value| -> std::collections::BTreeSet<String> {
            v.as_object().unwrap().keys().cloned().collect()
        };
        // Identical key set: the pre-unification optionals still serialize
        // (as null), and no personal-board key leaks into an orchestrated
        // file — the new fields are all skipped at their defaults.
        assert_eq!(keys(&out), keys(&fixture));
        assert_eq!(out["status"], "review");
        assert_eq!(out["project_id"], "p-fixture");
    }

    #[cfg(unix)]
    #[test]
    fn update_task_state_enforces_transitions() {
        crate::test_util::with_temp_home(|| {
            let state = TaskState::new(
                "p-transitions".into(),
                PathBuf::from("/tmp/p-transitions"),
                "do the thing".into(),
            );
            write_task_state(&state).unwrap();
            let pid = state.project_id.as_deref().unwrap();

            // Running → Merging is illegal and must not be persisted.
            // (Running → Backlog is now legal — it's the spawn-failure
            // claim rollback — so pick an edge that's still forbidden.)
            let err = update_task_state(pid, &state.task_id, |s| {
                s.status = TaskStatus::Merging;
            })
            .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            let on_disk = read_task_state(pid, &state.task_id).unwrap();
            assert_eq!(on_disk.status, TaskStatus::Running);

            // Running → Review is legal.
            let updated = update_task_state(pid, &state.task_id, |s| {
                s.status = TaskStatus::Review;
            })
            .unwrap();
            assert_eq!(updated.status, TaskStatus::Review);
        });
    }
}

/// In-place update under `read → mutate → write`, serialized by the
/// per-task advisory lock so concurrent writers (TUI, CLI, daemons) can't
/// lose each other's updates. The closure receives a mutable state and is
/// responsible for any field-level changes; `touch()` is called
/// automatically after the closure so callers don't have to remember.
pub fn update_task_state<F>(project_id: &str, task_id: &str, f: F) -> io::Result<TaskState>
where
    F: FnOnce(&mut TaskState),
{
    update_task_state_inner(Some(project_id), task_id, true, f)
}

/// Locked read-mutate-write against a personal-board task
/// (`~/.cc-hub/tasks/<tid>/state.json`). Same lock + transition enforcement
/// as the orchestrated wrapper; the kind axis (from the state itself)
/// selects the personal edge set.
pub fn update_personal_task<F>(task_id: &str, f: F) -> io::Result<TaskState>
where
    F: FnOnce(&mut TaskState),
{
    update_task_state_inner(None, task_id, true, f)
}

/// [`update_task_state`] without the trailing `touch()`. For background
/// daemons (triage, auto-review) that stamp bookkeeping timestamps and must
/// not reshuffle the kanban's `updated_at` ordering on every tick.
pub fn update_task_state_no_touch<F>(project_id: &str, task_id: &str, f: F) -> io::Result<TaskState>
where
    F: FnOnce(&mut TaskState),
{
    update_task_state_inner(Some(project_id), task_id, false, f)
}

/// Persist a Haiku-generated short title onto a task's state file (either
/// flavor). Reuses the per-task atomic-write store rather than a side cache
/// file so the title travels with the rest of the task state.
pub fn set_task_title(
    project_id: Option<&str>,
    task_id: &str,
    title: &str,
) -> io::Result<TaskState> {
    update_task_state_inner(project_id, task_id, true, |s| {
        s.title = Some(title.to_string());
    })
}

/// Remove every `<root>/.cc-hub-wt/<task>-<name>` worktree recorded for
/// `state`. `git worktree remove --force` deletes the worktree dir and its
/// admin entry but leaves the local `cc-hub/<task>-<name>` branch behind —
/// branches must be deleted separately (see `gc.rs`, which does the `git
/// branch -D` this function deliberately skips). Best-effort: failures are
/// logged and the loop continues.
pub fn remove_task_worktrees(state: &TaskState) {
    // Worktrees only exist for orchestrated tasks; a personal task has no
    // project root to run git in.
    let Some(project_root) = state.project_root.as_deref() else {
        return;
    };
    for w in &state.workers {
        let Some(name) = w.worktree.as_deref() else {
            continue;
        };
        let path = worktree_path(project_root, &state.task_id, name);
        match run_git(
            project_root,
            &["worktree", "remove", "--force", &path.to_string_lossy()],
        ) {
            Err(e) => log::warn!(
                "task {}: git worktree remove [{}] errored: {}",
                state.task_id,
                path.display(),
                e
            ),
            Ok(out) if !out.status_ok => log::warn!(
                "task {}: git worktree remove [{}] failed: {}",
                state.task_id,
                path.display(),
                out.stderr.trim()
            ),
            Ok(_) => {}
        }
    }
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
                state.task_id,
                w.tmux_name,
                e
            );
        }
    }
    if state.status == TaskStatus::Done {
        remove_task_worktrees(state);
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
                state.task_id,
                orch
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
                state.task_id,
                orch
            ),
            Err(e) => log::warn!(
                "task {}: schedule orchestrator kill failed: {}",
                state.task_id,
                e
            ),
        }
    }
}

fn capture_orchestrator_log(state: &TaskState, orch: &str) {
    let Some(pid) = state.project_id.as_deref() else {
        return;
    };
    let Some(path) = task_orchestrator_log_path(pid, &state.task_id) else {
        return;
    };
    let Some(dir) = path.parent() else { return };
    if let Err(e) = std::fs::create_dir_all(dir) {
        log::warn!(
            "task {}: orchestrator.log mkdir failed: {}",
            state.task_id,
            e
        );
        return;
    }
    let body = crate::send::capture_tmux_pane_full(orch);
    if body.is_empty() {
        log::warn!(
            "task {}: orchestrator capture-pane returned empty for [{}]",
            state.task_id,
            orch
        );
        return;
    }
    if let Err(e) = std::fs::write(&path, body) {
        log::warn!(
            "task {}: write orchestrator.log failed: {}",
            state.task_id,
            e
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_cmd: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectsFile {
    #[serde(default, rename = "project")]
    pub projects: Vec<Project>,
}

/// Look up the per-project build command from `~/.cc-hub/projects.toml`.
/// Returns `None` if the project isn't registered or has no `build_cmd` set;
/// callers fall back to their own default.
pub fn project_build_cmd(project_id: &str) -> Option<String> {
    load_projects()
        .projects
        .into_iter()
        .find(|p| p.id == project_id)
        .and_then(|p| p.build_cmd)
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

/// Cross-process advisory lock guarding read-modify-write of the projects
/// registry. Held across `load_projects` → mutate → `save_projects` so two
/// concurrent `task create` runs in different unregistered directories can't
/// lose one registration to last-writer-wins on the atomic rename. Mirrors
/// [`lock_task_state`], but for the single shared `projects.toml`.
///
/// The lock lives in a dedicated `projects.toml.lock` file (flock follows the
/// inode, and the tempfile+rename store can't be locked directly). Unlike the
/// per-task lock this never bails on a missing target: first-registration
/// races happen precisely when `projects.toml` doesn't exist yet, so we create
/// `~/.cc-hub` on demand and always take the lock. Pure readers
/// ([`load_projects`], [`project_build_cmd`]) stay lock-free.
fn lock_projects() -> io::Result<Option<fs::File>> {
    use fs2::FileExt;
    let Some(path) = projects_toml_path() else {
        return Ok(None);
    };
    let Some(parent) = path.parent() else {
        return Ok(None);
    };
    fs::create_dir_all(parent)?;
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(parent.join("projects.toml.lock"))?;
    f.lock_exclusive()?;
    Ok(Some(f))
}

/// Read-modify-write the projects registry under [`lock_projects`]. The
/// registry is (re)loaded *inside* the lock so a racing writer's committed
/// change is visible before `f` runs. `f` mutates the loaded registry and
/// returns `(persist, value)`: the mutated file is written back only when
/// `persist` is `true`, and `value` is handed to the caller. Every registry
/// mutation must route through here.
fn update_projects<T>(
    f: impl FnOnce(&mut ProjectsFile) -> io::Result<(bool, T)>,
) -> io::Result<T> {
    let _lock = lock_projects()?;
    let mut file = load_projects();
    let (persist, value) = f(&mut file)?;
    if persist {
        save_projects(&file)?;
    }
    Ok(value)
}

/// Register `root` if it isn't already, returning the project id either
/// way. `name` is used only when inserting a new entry.
pub fn ensure_project_registered(root: &Path, name: &str) -> io::Result<String> {
    let id = project_id_for_path(root);
    let id_for_closure = id.clone();
    update_projects(move |file| {
        if file.projects.iter().any(|p| p.id == id_for_closure) {
            return Ok((false, ()));
        }
        let canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        file.projects.push(Project {
            id: id_for_closure,
            name: name.to_string(),
            root: canon,
            created_at: now_unix_secs(),
            build_cmd: None,
        });
        Ok((true, ()))
    })?;
    Ok(id)
}

/// Remove a registered project from `~/.cc-hub/projects.toml` and delete
/// its on-disk task state directory (`~/.cc-hub/projects/<id>`). Returns
/// `Ok(())` if the project was removed (or wasn't present), or an error if
/// any orchestrator for this project is still alive — the caller surfaces
/// that to the user so they can clean up tasks first.
pub fn remove_project(project_id: &str) -> io::Result<()> {
    let proj_dir = project_state_dir(project_id);
    let tasks_dir = proj_dir.as_ref().map(|d| d.join("tasks"));

    // Registry read-check-write under the lock so a concurrent register /
    // remove can't clobber our retain. The liveness scan lives inside the
    // lock too — it gates whether we drop the entry at all. Returns whether
    // the entry was actually removed.
    let removed = update_projects(|file| {
        if !file.projects.iter().any(|p| p.id == project_id) {
            return Ok((false, false));
        }
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
        Ok((true, true))
    })?;

    // Not registered — leave any stray state dir untouched, matching the
    // original early-return.
    if !removed {
        return Ok(());
    }

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

/// Outcome of a `delete_task` call. All boolean fields are best-effort: a
/// `false` on `orchestrator_killed` means we tried and failed (logged),
/// `lock_released` is `false` when no lock was held by this task (or release
/// failed — logged), and `worktree_errors` lists the per-worktree paths
/// whose cleanup also failed the `fs::remove_dir_all` fallback. The state
/// directory removal is the only step that propagates as a hard error
/// (because if it fails, the task stays visible in the Projects view
/// forever).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedTask {
    pub task_id: String,
    pub project_id: String,
    pub orchestrator_killed: bool,
    pub lock_released: bool,
    pub state_removed: bool,
    pub worktrees_removed: Vec<String>,
    pub worktree_errors: Vec<(String, String)>,
}

/// Delete a task end-to-end: kill its orchestrator tmux, remove every
/// worktree the task owns (best-effort, falling back to plain rm -rf if
/// `git worktree remove --force` doesn't take), kill worker tmuxes, then
/// remove the on-disk state dir.
///
/// Returns `NotFound` (propagated from `read_task_state`) when the task
/// doesn't exist so callers can distinguish "already gone" from "deletion
/// failed". This is the mechanical primitive — status gating (Merging /
/// Running / `--force`) lives in the CLI verb and TUI.
pub fn delete_task(project_id: &str, task_id: &str) -> io::Result<DeletedTask> {
    let state = read_task_state(project_id, task_id)?;

    let mut orchestrator_killed = false;
    if let Some(name) = state.orchestrator_tmux.as_deref() {
        match crate::send::kill_tmux_session(name) {
            Ok(()) => orchestrator_killed = true,
            Err(e) => log::warn!(
                "delete_task {}: kill orchestrator tmux [{}] failed: {}",
                task_id,
                name,
                e
            ),
        }
    }

    let lock_released = match crate::merge_lock::release(project_id, &state.task_id) {
        Ok(released) => released,
        Err(e) => {
            log::warn!("delete_task {}: merge_lock release failed: {}", task_id, e);
            false
        }
    };

    let mut seen = std::collections::HashSet::new();
    let mut worktrees_removed = Vec::new();
    let mut worktree_errors: Vec<(String, String)> = Vec::new();
    // Callers pass a project id, so an orchestrated state always has a root;
    // guard anyway so a hand-edited state file degrades to "no worktrees".
    let project_root = state.project_root.clone().unwrap_or_default();
    for w in &state.workers {
        let Some(wt_name) = w.worktree.as_ref() else {
            continue;
        };
        let path = worktree_path(&project_root, &state.task_id, wt_name);
        let path_str = path.to_string_lossy().into_owned();
        if !seen.insert(path_str.clone()) {
            continue;
        }
        let path_arg = path.to_string_lossy().into_owned();
        let git_result = run_git(&project_root, &["worktree", "remove", "--force", &path_arg]);
        let counted = match &git_result {
            Ok(out) => {
                let stderr_lower = out.stderr.to_lowercase();
                let not_a_wt = stderr_lower.contains("not a working tree");
                out.status_ok || !path.exists() || not_a_wt
            }
            Err(_) => !path.exists(),
        };
        if counted {
            worktrees_removed.push(path_str);
            continue;
        }
        match fs::remove_dir_all(&path) {
            Ok(()) => worktrees_removed.push(path_str),
            Err(e) if e.kind() == io::ErrorKind::NotFound => worktrees_removed.push(path_str),
            Err(e) => {
                let prefix = match git_result {
                    Ok(out) => format!("git: {}", out.stderr.trim()),
                    Err(ge) => format!("git invoke: {}", ge),
                };
                worktree_errors.push((path_str, format!("{}; fs: {}", prefix, e)));
            }
        }
    }

    for w in &state.workers {
        if let Err(e) = crate::send::kill_tmux_session(&w.tmux_name) {
            log::warn!(
                "delete_task {}: kill worker tmux [{}] failed: {}",
                task_id,
                w.tmux_name,
                e
            );
        }
    }

    let state_dir =
        task_state_dir(project_id, task_id).ok_or_else(|| io::Error::other("no home dir"))?;
    let state_removed = match fs::remove_dir_all(&state_dir) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => return Err(e),
    };

    Ok(DeletedTask {
        task_id: task_id.to_string(),
        project_id: project_id.to_string(),
        orchestrator_killed,
        lock_released,
        state_removed,
        worktrees_removed,
        worktree_errors,
    })
}

/// Load every task's state for `project_id`. Skips entries whose `state.json`
/// is missing or unparseable (logged) rather than failing the whole scan, so
/// one corrupt task can't blind a gc / list pass. Returns an empty vec when
/// the project has no tasks dir yet.
pub fn list_task_states(project_id: &str) -> io::Result<Vec<TaskState>> {
    let Some(tasks_dir) = project_state_dir(project_id).map(|d| d.join("tasks")) else {
        return Ok(Vec::new());
    };
    if !tasks_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&tasks_dir)? {
        let entry = entry?;
        let state_path = entry.path().join("state.json");
        let raw = match fs::read_to_string(&state_path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                log::warn!(
                    "list_task_states: read {} failed: {}",
                    state_path.display(),
                    e
                );
                continue;
            }
        };
        match serde_json::from_str::<TaskState>(&raw) {
            Ok(s) => out.push(s),
            Err(e) => log::warn!(
                "list_task_states: parse {} failed: {}",
                state_path.display(),
                e
            ),
        }
    }
    Ok(out)
}

/// Task ids under `<project>/tasks/` whose `state.json` is PRESENT but failed
/// to parse. These are distinct from absent tasks: a genuinely live task with a
/// momentarily/persistently corrupt state would otherwise vanish from
/// [`list_task_states`], so [`scan_worktrees`] must treat them as live and
/// never gc their (possibly in-flight) worktrees. Best-effort: any IO error
/// just yields a smaller set (fewer keep-alives), never a hard failure.
pub(super) fn unparsed_task_ids(project_id: &str) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::new();
    let Some(tasks_dir) = project_state_dir(project_id).map(|d| d.join("tasks")) else {
        return ids;
    };
    let Ok(rd) = fs::read_dir(&tasks_dir) else {
        return ids;
    };
    for entry in rd.flatten() {
        let state_path = entry.path().join("state.json");
        let Ok(raw) = fs::read_to_string(&state_path) else {
            continue; // absent state → handled as orphan elsewhere, not here
        };
        if serde_json::from_str::<TaskState>(&raw).is_err() {
            if let Some(id) = entry.file_name().to_str() {
                ids.insert(id.to_owned());
            }
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::HOME_TEST_LOCK;

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
            agent_id: "claude".into(),
            agent_kind: AgentKind::Claude,
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

        let result =
            set_task_title(Some(&project_id), &task_id_set, "build thing").expect("set_task_title");
        assert_eq!(result.title.as_deref(), Some("build thing"));

        let loaded = read_task_state(&project_id, &task_id_set).expect("read state back from disk");
        assert_eq!(loaded.title.as_deref(), Some("build thing"));
        assert!(
            loaded.updated_at >= loaded.created_at,
            "touch() should bump updated_at"
        );

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
    }

    #[test]
    fn task_status_backlog_serialises_lowercase() {
        assert_eq!(
            serde_json::to_string(&TaskStatus::Backlog).unwrap(),
            "\"backlog\""
        );
    }

    #[test]
    fn task_status_merging_serialises_lowercase() {
        assert_eq!(
            serde_json::to_string(&TaskStatus::Merging).unwrap(),
            "\"merging\""
        );
        let parsed: TaskStatus = serde_json::from_str("\"merging\"").unwrap();
        assert_eq!(parsed, TaskStatus::Merging);
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
    fn task_id_format() {
        let id = new_task_id();
        assert!(id.starts_with("t-"));
        assert!(id.len() > 4);
    }

    #[test]
    fn remove_project_deletes_registry_entry_and_state_dir() {
        let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let project_root = home.path().join("proj-under-test");
        fs::create_dir_all(&project_root).expect("mkdir project root");
        let project_id = ensure_project_registered(&project_root, "proj").expect("register");

        // Seed a task so the project state dir actually exists on disk.
        let mut state = TaskState::new(project_id.clone(), project_root.clone(), "do thing".into());
        state.orchestrator_tmux = None;
        write_task_state(&state).expect("write seed task state");

        let proj_dir = project_state_dir(&project_id).expect("project_state_dir");
        assert!(
            proj_dir.exists(),
            "project state dir should exist after seed"
        );

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

    #[test]
    fn concurrent_registrations_all_survive() {
        // Without the projects.toml lock, N racing registrations in distinct
        // unregistered dirs lose all but one to last-writer-wins on the
        // atomic rename. The advisory lock must serialise them so every
        // registration lands. flock contends across the separate FDs each
        // thread opens, so this exercises the real cross-writer path.
        let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let n: usize = 8;
        let mut handles = Vec::new();
        for i in 0..n {
            let root = home.path().join(format!("proj-{}", i));
            fs::create_dir_all(&root).expect("mkdir");
            handles.push(std::thread::spawn(move || {
                ensure_project_registered(&root, &format!("p{}", i)).expect("register");
            }));
        }
        for h in handles {
            h.join().expect("join");
        }

        let after = load_projects();
        assert_eq!(
            after.projects.len(),
            n,
            "every concurrent registration must survive the lock"
        );

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    /// Seed enough state for a `delete_task` test: swap HOME to a fresh
    /// tempdir, register a project under `root_name`, write a single
    /// `TaskState`. Returns the tempdir (keep alive for the test lifetime),
    /// project id, and task id. Caller is responsible for taking
    /// `HOME_TEST_LOCK` and restoring the prior HOME afterwards.
    fn seed_delete_test(root_name: &str) -> (tempfile::TempDir, String, String) {
        let home = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HOME", home.path());
        let project_root = home.path().join(root_name);
        fs::create_dir_all(&project_root).expect("mkdir project root");
        let project_id = ensure_project_registered(&project_root, "proj").expect("register");
        let mut state = TaskState::new(project_id.clone(), project_root, "do thing".into());
        state.orchestrator_tmux = None;
        let task_id = state.task_id.clone();
        write_task_state(&state).expect("write seed task state");
        (home, project_id, task_id)
    }

    fn restore_home(prev: Option<std::ffi::OsString>) {
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn delete_task_releases_merge_lock_when_held() {
        let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let (_home, project_id, task_id) = seed_delete_test("proj-lock-held");

        match crate::merge_lock::acquire(&project_id, &task_id, None).expect("acquire lock") {
            crate::merge_lock::AcquireOutcome::Acquired => {}
            other => panic!("expected Acquired, got {:?}", other),
        }

        let deleted = delete_task(&project_id, &task_id).expect("delete_task");
        assert!(
            deleted.lock_released,
            "lock_released should be true when this task held the lock"
        );
        assert!(
            crate::merge_lock::current_holder(&project_id)
                .expect("current_holder")
                .is_none(),
            "lock should be released after delete"
        );

        restore_home(prev_home);
    }

    #[test]
    fn delete_task_no_lock_held_returns_false() {
        let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let (_home, project_id, task_id) = seed_delete_test("proj-no-lock");

        let state_dir = task_state_dir(&project_id, &task_id).expect("task_state_dir");
        let deleted = delete_task(&project_id, &task_id).expect("delete_task");
        assert!(
            !deleted.lock_released,
            "lock_released should be false when no lock was held"
        );
        assert!(deleted.state_removed, "rest of delete should still succeed");
        assert!(!state_dir.exists(), "state dir should be gone after delete");

        restore_home(prev_home);
    }

    /// Seed a tempdir as a git repo with one commit and one worktree, then
    /// build a `TaskState` referencing that worktree under the given status.
    /// Returns the tempdir (to keep it alive), the worktree path, and the
    /// state.
    fn seed_repo_and_state(status: TaskStatus) -> (tempfile::TempDir, PathBuf, TaskState) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        run_git(root, &["init", "-q"]).expect("git init");
        run_git(root, &["config", "user.email", "test@example.com"]).expect("config email");
        run_git(root, &["config", "user.name", "Test"]).expect("config name");
        fs::write(root.join("seed.txt"), b"seed").expect("write seed");
        run_git(root, &["add", "seed.txt"]).expect("git add");
        run_git(root, &["commit", "-q", "-m", "seed"]).expect("git commit");
        let base = detect_main_branch(root);

        let wt = create_worktree(root, "t-test", "edit", &base).expect("create_worktree");

        let mut state = TaskState::new("p".into(), root.to_path_buf(), "do thing".into());
        state.task_id = "t-test".into();
        state.status = status;
        state.orchestrator_tmux = None;
        state.workers.push(Worker {
            agent_id: "claude".into(),
            agent_kind: AgentKind::Claude,
            tmux_name: "nonexistent-test-session".into(),
            cwd: wt.clone(),
            worktree: Some("edit".into()),
            readonly: false,
            spawned_at: 0,
        });
        (tmp, wt, state)
    }

    #[test]
    fn cleanup_removes_worktrees_when_done() {
        let (_tmp, wt, state) = seed_repo_and_state(TaskStatus::Done);
        cleanup_task_sessions(&state);
        assert!(
            !wt.exists(),
            "worktree dir should be gone after cleanup: {}",
            wt.display()
        );
    }

    #[test]
    fn cleanup_skips_worktree_removal_when_not_done() {
        let (_tmp, wt, state) = seed_repo_and_state(TaskStatus::Running);
        cleanup_task_sessions(&state);
        assert!(
            wt.exists(),
            "worktree dir must survive cleanup while task is still Running: {}",
            wt.display()
        );
    }
}
