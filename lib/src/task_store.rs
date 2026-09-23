//! On-disk store for Tasks-board cards: one `state.json` per task under
//! `~/.cc-hub/tasks/<task-id>/`, next to a `state.lock` that serializes every
//! read-mutate-write across processes (TUI, `cc-hub board`, deep links).
//! Writes are atomic (tempfile + rename), and every status change passes
//! [`validate_status_transition`], the board's single legal-edge table.
//!
//! Task ID format: `tk-<unix-nanos>`. Sortable, unique within a single host
//! to nanosecond resolution, no extra dep.

use crate::platform::paths::cc_hub_home;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Root of the task store: `~/.cc-hub/tasks/`.
pub fn tasks_dir() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("tasks"))
}

pub fn task_dir(task_id: &str) -> Option<PathBuf> {
    tasks_dir().map(|d| d.join(task_id))
}

pub fn task_state_file(task_id: &str) -> Option<PathBuf> {
    task_dir(task_id).map(|d| d.join("state.json"))
}

pub fn new_task_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("tk-{}", nanos)
}

/// Compact rendering of a task id for in-card display: its last 6 digits,
/// unique within the active set without dominating the badge.
pub fn short_task_id(task_id: &str) -> String {
    let take = task_id.len().saturating_sub(6);
    task_id[take..].to_string()
}

pub fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A card's column. Backlog ("To-Do") → Planning → Running ("In Progress")
/// → Review → Done; see [`validate_status_transition`] for every legal edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Backlog,
    /// An agent was assigned and told to present a plan first; the card
    /// waits for the user to approve it (Space → Running).
    Planning,
    Running,
    /// The card's session wrote a `PR:` note, which carries the card here
    /// (see [`crate::ops::task::task_artifact_add_text`]) — so the board
    /// separates a card that wants a review from one that wants an answer.
    Review,
    Done,
}

impl TaskStatus {
    /// Lowercase wire name. Must match `#[serde(rename_all = "lowercase")]`
    /// above so JSON round-trips agree.
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Backlog => "backlog",
            TaskStatus::Planning => "planning",
            TaskStatus::Running => "running",
            TaskStatus::Review => "review",
            TaskStatus::Done => "done",
        }
    }

    /// Human label as shown on the Tasks-board columns ("To-Do",
    /// "In Progress", …). Distinct from [`Self::as_str`], the wire name.
    pub fn board_label(&self) -> &'static str {
        match self {
            TaskStatus::Backlog => "To-Do",
            TaskStatus::Planning => "Planning",
            TaskStatus::Running => "In Progress",
            TaskStatus::Review => "Review",
            TaskStatus::Done => "Done",
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
            "done" => Ok(TaskStatus::Done),
            _ => Err(()),
        }
    }
}

/// Task priority (P1 highest … P4 lowest). Variants are declared in
/// ascending order (`P1 < P2 < P3 < P4`) so a plain ascending sort puts the
/// most urgent first. `P3` (the default) is skipped during serialization.
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

/// A note or file attached to a card — pasted text, a screenshot, a URL.
/// Stored alongside the task state. `kind` is free-form by design.
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

/// One board card. State files written by older builds may carry keys this
/// struct no longer has; serde ignores them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskState {
    pub task_id: String,
    /// When the task landed in Done, for the board's Done column stamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<i64>,
    #[serde(default, skip_serializing_if = "TaskPriority::is_default")]
    pub priority: TaskPriority,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// The deliverable this card produces — `tps`, `ai-plugin`, `hub`, … —
    /// chosen on the board (`T`) from `[tasks].kinds`. The task router places
    /// a card by this word instead of inferring one from the text, and a card
    /// that carries it is never handed back unrouted. `None` leaves the
    /// classification to the router.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Where the assigned agent runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Agent id of the assignment (e.g. "claude").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Mux session of the assigned agent, from spawn time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux: Option<String>,
    /// Agent session id, resolved from the first scan that sees the spawned
    /// tmux. Outlives the tmux session, so `f` can resume after it dies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// All sessions assigned over this task's lifetime, including reassignments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub usage_session_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<crate::task_stats::TaskStats>,
    pub status: TaskStatus,
    /// The card's text as the user typed it.
    pub prompt: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// 2-3 word Haiku-generated title derived from the prompt. Mirrors
    /// `SessionInfo::title`. `None` until the background titler finishes
    /// (or if it fails).
    #[serde(default)]
    pub title: Option<String>,
    /// Notes and files attached over the task's lifetime.
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    /// Index into `artifacts` of the single "lead" artifact — the strongest
    /// piece of proof, surfaced first. `None` until set.
    #[serde(default)]
    pub lead_artifact: Option<usize>,
}

impl TaskState {
    /// A fresh To-Do card.
    pub fn new(prompt: String) -> Self {
        let now = now_unix_secs();
        Self {
            task_id: new_task_id(),
            done_at: None,
            priority: TaskPriority::default(),
            tags: Vec::new(),
            kind: None,
            cwd: None,
            agent_id: None,
            tmux: None,
            session_id: None,
            usage_session_ids: Vec::new(),
            stats: None,
            status: TaskStatus::Backlog,
            prompt,
            created_at: now,
            updated_at: now,
            title: None,
            artifacts: Vec::new(),
            lead_artifact: None,
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = now_unix_secs();
    }

    /// The name a session working this card is born with: `Task: <title>`,
    /// or `Task: <prompt>` while the card has no title yet.
    pub fn session_title(&self) -> String {
        crate::link::titled("Task", self.title.as_deref().unwrap_or(&self.prompt))
    }
}

/// Read a task state file; missing file returns NotFound, parse errors
/// surface as InvalidData so callers can distinguish "no such task" from
/// "schema drift".
pub fn read_task_state(task_id: &str) -> io::Result<TaskState> {
    let path = task_state_file(task_id).ok_or_else(|| io::Error::other("no home dir"))?;
    let raw = fs::read_to_string(&path)?;
    serde_json::from_str(&raw).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {}", path.display(), e),
        )
    })
}

/// Atomically write a task state file via tempfile + rename. Creates parent
/// dirs on demand.
pub fn write_task_state(state: &TaskState) -> io::Result<()> {
    let path = task_state_file(&state.task_id).ok_or_else(|| io::Error::other("no home dir"))?;
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
/// read-mutate-write over this task's `state.json` across processes. The
/// lock lives in a dedicated `state.lock` file next to `state.json` because
/// flock follows the inode — a tempfile+rename store can't be locked
/// directly. Returns `None` when the task directory doesn't exist yet:
/// there's nothing to protect, and the caller's read will surface
/// `NotFound` with its usual error.
pub(crate) fn lock_task_state(task_id: &str) -> io::Result<Option<fs::File>> {
    use fs2::FileExt;
    let Some(dir) = task_dir(task_id) else {
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

/// Single source of truth for legal task-status transitions. Enforced
/// centrally by [`update_task`], so no CLI verb or TUI keybind can invent an
/// edge the board doesn't have. Self-transitions are always allowed.
///
/// | from → to           | produced by                                         |
/// |---------------------|-----------------------------------------------------|
/// | Backlog ↔ Running   | manual move                                         |
/// | Backlog ↔ Planning  | assign (`s`/`S`) / manual move back                 |
/// | Running → Planning  | re-assign a stalled In-Progress card                |
/// | Done → Planning     | re-assign a finished card (reopen with agent)       |
/// | Planning → Running  | plan approved (Space), manual move                  |
/// | Running → Done      | finish                                              |
/// | Backlog → Done      | Space checks off a To-Do card directly              |
/// | Planning → Done     | finish an assigned card without approving the plan  |
/// | Done → Backlog      | reopen (Space on a Done card)                       |
/// | Done → Running      | manual move off Done                                |
/// | Running → Review    | a `PR:` note                                        |
/// | Planning → Review   | a `PR:` note from a card still in the plan gate     |
/// | Review  → Running   | manual move                                         |
/// | Review  → Planning  | re-assign a card whose PR needs another round       |
/// | Review  → Done      | finish                                              |
/// | Done → Review       | manual move off Done                                |
pub fn validate_status_transition(from: &TaskStatus, to: &TaskStatus) -> Result<(), String> {
    use TaskStatus::*;
    let legal = from == to
        || matches!(
            (from, to),
            (Backlog, Running)
                | (Running, Backlog)
                | (Running, Done)
                | (Backlog, Planning)
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
                // The review gate: a `PR:` note carries the card here,
                // and it leaves either finished or back into another
                // round of work.
                | (Running, Review)
                | (Planning, Review)
                | (Review, Running)
                | (Review, Planning)
                | (Review, Done)
                | (Done, Review)
        );
    if legal {
        Ok(())
    } else {
        Err(format!(
            "illegal task status transition {:?} → {:?} (the board flows Backlog → Planning → \
             Running → Review → Done; Review can bounce back to Running/Planning, and Done can \
             reopen to Backlog/Running/Review)",
            from, to
        ))
    }
}

fn update_task_inner<F>(task_id: &str, touch: bool, f: F) -> io::Result<TaskState>
where
    F: FnOnce(&mut TaskState),
{
    try_update_task_inner(task_id, touch, |s| {
        let previous_session = s.session_id.clone();
        f(s);
        for sid in previous_session.iter().chain(s.session_id.iter()) {
            if !s.usage_session_ids.contains(sid) {
                s.usage_session_ids.push(sid.clone());
            }
        }
        true
    })
    .map(|(state, _)| state)
}

fn try_update_task_inner<F>(task_id: &str, touch: bool, f: F) -> io::Result<(TaskState, bool)>
where
    F: FnOnce(&mut TaskState) -> bool,
{
    let _lock = lock_task_state(task_id)?;
    let mut state = read_task_state(task_id)?;
    let prev_status = state.status;
    if !f(&mut state) {
        return Ok((state, false));
    }
    if state.status != prev_status {
        validate_status_transition(&prev_status, &state.status)
            .map_err(|msg| io::Error::new(io::ErrorKind::InvalidInput, msg))?;
    }
    if touch {
        state.touch();
    }
    write_task_state(&state)?;
    if state.status != prev_status {
        // A card changed column. Agents that watch the board — the task
        // router above all — poll on the next second instead of waiting out
        // their interval. Best-effort by design: the wake says only "look
        // now", so one that never lands costs latency, not a routing.
        if let Some(wake) = crate::wake::Wake::named(crate::wake::BOARD) {
            if let Err(e) = wake.now() {
                log::warn!("board wake: {}", e);
            }
        }
    }
    Ok((state, true))
}

/// In-place update under `read → mutate → write`, serialized by the
/// per-task advisory lock so concurrent writers can't lose each other's
/// updates. `touch()` is called automatically after the closure.
pub fn update_task<F>(task_id: &str, f: F) -> io::Result<TaskState>
where
    F: FnOnce(&mut TaskState),
{
    update_task_inner(task_id, true, f)
}

/// Save usage without changing the task's activity timestamp.
pub fn set_task_stats(task_id: &str, stats: crate::task_stats::TaskStats) -> io::Result<TaskState> {
    update_task_inner(task_id, false, |s| s.stats = Some(stats))
}

/// Persist a Haiku-generated short title onto a task's state file, so the
/// title travels with the rest of the task state.
pub fn set_task_title(task_id: &str, title: &str) -> io::Result<TaskState> {
    update_task_inner(task_id, true, |s| {
        s.title = Some(title.to_string());
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legal_edges_pass() {
        use TaskStatus::*;
        for (from, to) in [
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
            // The review gate: a `PR:` note carries the card in, and it
            // leaves finished or into another round.
            (Running, Review),
            (Planning, Review),
            (Review, Running),
            (Review, Planning),
            (Review, Done),
            (Done, Review),
        ] {
            assert!(
                validate_status_transition(&from, &to).is_ok(),
                "{:?} → {:?} should be legal",
                from,
                to
            );
        }
    }

    #[test]
    fn illegal_edges_fail() {
        use TaskStatus::*;
        for (from, to) in [(Backlog, Review), (Review, Backlog)] {
            assert!(
                validate_status_transition(&from, &to).is_err(),
                "{:?} → {:?} should be illegal",
                from,
                to
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn update_task_enforces_transitions() {
        crate::test_util::with_temp_home(|| {
            let state = TaskState::new("do the thing".into());
            write_task_state(&state).unwrap();

            // Backlog → Review is illegal and must not be persisted.
            let err = update_task(&state.task_id, |s| s.status = TaskStatus::Review).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            let on_disk = read_task_state(&state.task_id).unwrap();
            assert_eq!(on_disk.status, TaskStatus::Backlog);

            // Backlog → Running is legal.
            let updated = update_task(&state.task_id, |s| s.status = TaskStatus::Running).unwrap();
            assert_eq!(updated.status, TaskStatus::Running);
        });
    }

    #[test]
    fn task_state_with_artifacts_round_trips() {
        let mut s = TaskState::new("do the thing".into());
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

    /// A state.json written by an older build carries keys this struct
    /// dropped (the Projects layer's orchestrator/worker fields). It must
    /// still load, with the missing optional fields defaulted.
    #[test]
    fn old_state_file_with_retired_keys_loads() {
        let raw = r#"{
            "task_id": "tk-1",
            "orchestrator_session_id": null,
            "orchestrator_agent_id": "claude",
            "orchestrator_agent_kind": "claude",
            "orchestrator_tmux": null,
            "status": "running",
            "prompt": "hi",
            "created_at": 1,
            "updated_at": 2,
            "note": null,
            "summary": null,
            "workers": [],
            "merges": [],
            "todos": [],
            "triaged_at": null,
            "last_auto_reviewed_at": null,
            "shipped_version": null
        }"#;
        let s: TaskState = serde_json::from_str(raw).unwrap();
        assert_eq!(s.status, TaskStatus::Running);
        assert!(s.artifacts.is_empty());
        assert_eq!(s.lead_artifact, None);
        assert_eq!(s.title, None);
    }

    #[cfg(unix)]
    #[test]
    fn set_task_title_persists_through_round_trip() {
        crate::test_util::with_temp_home(|| {
            let initial = TaskState::new("build the thing".into());
            write_task_state(&initial).expect("write seed state");

            let result = set_task_title(&initial.task_id, "build thing").expect("set_task_title");
            assert_eq!(result.title.as_deref(), Some("build thing"));

            let loaded = read_task_state(&initial.task_id).expect("read state back from disk");
            assert_eq!(loaded.title.as_deref(), Some("build thing"));
            assert!(
                loaded.updated_at >= loaded.created_at,
                "touch() should bump updated_at"
            );
        });
    }

    #[test]
    fn task_status_serialises_lowercase() {
        assert_eq!(
            serde_json::to_string(&TaskStatus::Running).unwrap(),
            "\"running\""
        );
        assert_eq!(
            serde_json::to_string(&TaskStatus::Backlog).unwrap(),
            "\"backlog\""
        );
    }
}
