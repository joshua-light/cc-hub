//! Personal task board shown on the Tasks tab. Unlike the Projects-tab
//! orchestrator tasks (heavyweight: orchestrator session, workers, PR
//! pipeline), a board task is a plain to-do item that can optionally be
//! handed to a single agent session: assigning spawns a detached agent in a
//! chosen cwd, prompted to investigate and plan first — the card sits in
//! Planning until the user approves the plan (Space), which tells the agent
//! to proceed and moves the card to In Progress. The binding is recorded so
//! `f` on the card attaches to that session exactly like the Sessions tab.
//! Stored at `~/.cc-hub/tasks.json` as an ordered array so the file is
//! trivially editable by hand if needed.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::platform::paths::cc_hub_home;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskItemStatus {
    Todo,
    /// An agent is investigating the task and drafting a plan; it was told
    /// not to implement until the user approves (Space on the card).
    Planning,
    InProgress,
    Done,
}

/// Task priority, P1 (most urgent) through P4 (lowest). Drives the board's
/// within-column ordering: cards sort by priority first, so P1 floats to the
/// top of its column. Derived `Ord` follows declaration order
/// (`P1 < P2 < P3 < P4`), so a plain ascending sort puts the most urgent
/// first. New tasks (and any loaded from a pre-priority `tasks.json`) default
/// to `P3` — explicitly raising a task is what lifts it above the pack.
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

/// Longest a single tag may be after normalization; longer ones are truncated.
const MAX_TAG_LEN: usize = 16;
/// Most tags a task may carry; extras are dropped so the border badge fits.
const MAX_TAGS: usize = 6;

/// Parse a free-text tag line (from the inline editor) into the normalized set
/// stored on a task. Splits on whitespace *and* commas, strips a leading `#`,
/// lowercases, trims, drops empties, and dedupes (keeping first-seen order).
/// Each tag is capped at [`MAX_TAG_LEN`] chars and the set at [`MAX_TAGS`], so
/// the card's border badge always has a sane bound. Editing replaces the whole
/// set, so this is the single chokepoint for what a task's tags can be.
pub fn parse_tags(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in input.split([',', ' ', '\t', '\n']) {
        let tag = raw.trim().trim_start_matches('#').trim().to_lowercase();
        if tag.is_empty() {
            continue;
        }
        let tag: String = tag.chars().take(MAX_TAG_LEN).collect();
        if out.iter().any(|t| t == &tag) {
            continue;
        }
        out.push(tag);
        if out.len() >= MAX_TAGS {
            break;
        }
    }
    out
}

/// Parsed form of the add popup's quick syntax: `#word` tokens become tags
/// and a `!1`–`!4` token sets the priority; everything else stays as the
/// task text. Rename leaves text verbatim — the syntax is capture-time
/// sugar only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickAdd {
    pub text: String,
    pub tags: Vec<String>,
    pub priority: Option<TaskPriority>,
}

/// Split the add popup's buffer into text, tags, and priority. Tag tokens
/// run through [`parse_tags`], so the tag editor's normalization and caps
/// apply here too. Several `!N` tokens keep the last one; a bare `#` or an
/// out-of-range `!5` is ordinary text.
pub fn parse_quick_add(input: &str) -> QuickAdd {
    let mut words: Vec<&str> = Vec::new();
    let mut tag_words: Vec<&str> = Vec::new();
    let mut priority = None;
    for tok in input.split_whitespace() {
        if let Some(rest) = tok.strip_prefix('#') {
            if !rest.is_empty() {
                tag_words.push(rest);
                continue;
            }
        }
        match tok {
            "!1" => priority = Some(TaskPriority::P1),
            "!2" => priority = Some(TaskPriority::P2),
            "!3" => priority = Some(TaskPriority::P3),
            "!4" => priority = Some(TaskPriority::P4),
            _ => words.push(tok),
        }
    }
    QuickAdd {
        text: words.join(" "),
        tags: parse_tags(&tag_words.join(" ")),
        priority,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskItem {
    pub id: String,
    pub text: String,
    pub status: TaskItemStatus,
    /// Sort priority within the column. Absent in pre-priority boards, so
    /// `serde(default)` fills it with [`TaskPriority::default`] (`P3`).
    #[serde(default)]
    pub priority: TaskPriority,
    /// Short free-form labels shown as `#tag` badges on the card. Normalized
    /// by [`parse_tags`] (lowercased, deduped, capped). Omitted from the JSON
    /// when empty, and absent in pre-tags boards, so `serde(default)` fills it
    /// with an empty vec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<u64>,
    /// Working directory the agent was assigned in. Doubles as the picker's
    /// starting point on re-assign.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Mux session name returned by spawn — known synchronously at assign
    /// time, used to attach (`f`) and to resolve `session_id` from scans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux: Option<String>,
    /// Agent session id, resolved from the first scan that sees the spawned
    /// tmux. Outlives the tmux session, so `f` can resume after it dies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct TaskBoard {
    #[serde(default)]
    tasks: Vec<TaskItem>,
    /// cwd of the most recent assignment, kept across restarts. The assign
    /// picker promotes it to the top of the places list so firing several
    /// tasks at one project is a plain Enter each time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_assign_cwd: Option<String>,
    /// Monotonic compare-and-swap token. Older files deserialize as revision
    /// zero; every successful mutation increments it while holding the board
    /// lock, preventing two cc-hub instances from silently overwriting each
    /// other's changes.
    #[serde(default)]
    revision: u64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn new_task_item_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("tk-{}", nanos)
}

impl TaskBoard {
    /// Load the board from disk. Missing state is an empty board; malformed or
    /// unreadable state is an error so callers never mistake data loss for an
    /// intentionally empty board.
    pub fn load_result() -> io::Result<Self> {
        let Some(path) = tasks_path() else {
            return Ok(Self::default());
        };
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        serde_json::from_str(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Convenience for non-interactive callers and tests. Runtime UI code uses
    /// [`Self::load_result`] so it can surface failures.
    pub fn load() -> Self {
        Self::load_result().unwrap_or_default()
    }

    pub fn tasks(&self) -> &[TaskItem] {
        &self.tasks
    }

    pub fn get(&self, id: &str) -> Option<&TaskItem> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn last_assign_cwd(&self) -> Option<&str> {
        self.last_assign_cwd.as_deref()
    }

    /// Tasks in `status`, in insertion order.
    pub fn column(&self, status: TaskItemStatus) -> Vec<&TaskItem> {
        self.tasks.iter().filter(|t| t.status == status).collect()
    }

    /// Append a task (trimmed) to To-Do. Empty/whitespace-only input is
    /// ignored. Returns the new task's id. Persists immediately.
    pub fn add(&mut self, text: &str) -> io::Result<Option<String>> {
        self.add_configured(text, Vec::new(), TaskPriority::default())
    }

    /// Add a task and its quick-capture metadata in one repository commit, so
    /// a write failure cannot leave behind a card missing its tags/priority.
    pub fn add_configured(
        &mut self,
        text: &str,
        tags: Vec<String>,
        priority: TaskPriority,
    ) -> io::Result<Option<String>> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        let id = new_task_item_id();
        self.commit(|board| {
            board.tasks.push(TaskItem {
                id: id.clone(),
                text: text.to_string(),
                status: TaskItemStatus::Todo,
                priority,
                tags,
                created_at: now_secs(),
                done_at: None,
                cwd: None,
                agent_id: None,
                tmux: None,
                session_id: None,
            });
        })?;
        Ok(Some(id))
    }

    /// Replace a task's text (trimmed), leaving status and any agent
    /// binding untouched. Empty/whitespace-only input and unknown ids are
    /// ignored (returns false). Persists.
    pub fn rename(&mut self, id: &str, text: &str) -> io::Result<bool> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(false);
        }
        let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) else {
            return Ok(false);
        };
        if t.text == text {
            return Ok(false);
        }
        self.commit(|board| {
            board.tasks.iter_mut().find(|t| t.id == id).unwrap().text = text.into()
        })?;
        Ok(true)
    }

    /// Set a task's priority. Skips the disk write when the priority is
    /// unchanged (re-pressing the same level is a no-op). No-op on unknown
    /// id. Persists when it changes.
    pub fn set_priority(&mut self, id: &str, priority: TaskPriority) -> io::Result<bool> {
        if self.get(id).is_none_or(|t| t.priority == priority) {
            return Ok(false);
        }
        self.commit(|board| {
            board
                .tasks
                .iter_mut()
                .find(|t| t.id == id)
                .unwrap()
                .priority = priority
        })?;
        Ok(true)
    }

    /// Replace a task's tags with `tags` (already normalized by
    /// [`parse_tags`]). Skips the disk write when unchanged. No-op on unknown
    /// id. Persists when it changes.
    pub fn set_tags(&mut self, id: &str, tags: Vec<String>) -> io::Result<bool> {
        if self.get(id).is_none_or(|t| t.tags == tags) {
            return Ok(false);
        }
        self.commit(|board| board.tasks.iter_mut().find(|t| t.id == id).unwrap().tags = tags)?;
        Ok(true)
    }

    /// Move a task between columns, stamping/clearing `done_at` so the Done
    /// column can show when it landed. No-op on unknown id. Persists.
    pub fn set_status(&mut self, id: &str, status: TaskItemStatus) -> io::Result<bool> {
        if self.get(id).is_none_or(|t| t.status == status) {
            return Ok(false);
        }
        self.commit(|board| {
            let t = board.tasks.iter_mut().find(|t| t.id == id).unwrap();
            t.status = status;
            t.done_at = (status == TaskItemStatus::Done).then(now_secs);
        })?;
        Ok(true)
    }

    /// Record an agent assignment: where it runs, which agent, and the mux
    /// session it lives in. Moves the task to Planning — the agent is
    /// prompted to plan first and the user promotes the card to In Progress
    /// by approving the plan. Persists.
    pub fn assign(&mut self, id: &str, cwd: &str, agent_id: &str, tmux: &str) -> io::Result<bool> {
        if self.get(id).is_none() {
            return Ok(false);
        }
        self.commit(|board| {
            let t = board.tasks.iter_mut().find(|t| t.id == id).unwrap();
            t.cwd = Some(cwd.to_string());
            t.agent_id = Some(agent_id.to_string());
            t.tmux = Some(tmux.to_string());
            // A re-assign spawns a fresh session; the old session id no
            // longer matches the new tmux, so drop it until the next scan
            // re-resolves.
            t.session_id = None;
            t.status = TaskItemStatus::Planning;
            t.done_at = None;
            board.last_assign_cwd = Some(cwd.to_string());
        })?;
        Ok(true)
    }

    /// Point an existing assignment at a new mux session (resume after the
    /// old tmux died). Keeps `session_id` — resume continues that session.
    pub fn rebind_tmux(&mut self, id: &str, tmux: &str) -> io::Result<bool> {
        if self.get(id).is_none_or(|t| t.tmux.as_deref() == Some(tmux)) {
            return Ok(false);
        }
        self.commit(|board| {
            board.tasks.iter_mut().find(|t| t.id == id).unwrap().tmux = Some(tmux.into())
        })?;
        Ok(true)
    }

    /// Fill in `session_id` for any assigned task whose tmux name matches a
    /// scanned session. Returns true when something new was learned (and
    /// persisted) so callers can repaint.
    pub fn bind_sessions(&mut self, sessions: &[crate::models::SessionInfo]) -> io::Result<bool> {
        let bindings: Vec<(String, String)> = self
            .tasks
            .iter()
            .filter_map(|t| {
                if t.session_id.is_some() {
                    return None;
                }
                let tmux = t.tmux.as_deref()?;
                sessions
                    .iter()
                    .find(|s| s.tmux_session.as_deref() == Some(tmux))
                    .map(|s| (t.id.clone(), s.session_id.clone()))
            })
            .collect();
        if bindings.is_empty() {
            return Ok(false);
        }
        self.commit(|board| {
            for (id, sid) in &bindings {
                board
                    .tasks
                    .iter_mut()
                    .find(|t| t.id == *id)
                    .unwrap()
                    .session_id = Some(sid.clone());
            }
        })?;
        Ok(true)
    }

    /// Remove the task with `id`, returning it so the caller can describe
    /// what was deleted (and whether an agent session survives it). The
    /// removed task is appended to the on-disk archive. Persists.
    pub fn remove(&mut self, id: &str) -> io::Result<Option<TaskItem>> {
        let Some(idx) = self.tasks.iter().position(|t| t.id == id) else {
            return Ok(None);
        };
        let removed = self.tasks[idx].clone();
        // Archive first: an extra archive entry is harmless, while committing
        // the removal before a failed archive write would report failure after
        // the card had already disappeared.
        archive_tasks(std::slice::from_ref(&removed))?;
        self.commit(|board| {
            board.tasks.remove(idx);
        })?;
        Ok(Some(removed))
    }

    /// Re-insert a previously removed task (undo of `x`/`c`). Skips ids
    /// already on the board (double-undo, hand edits); returns whether it
    /// landed. Persists.
    pub fn restore(&mut self, item: TaskItem) -> io::Result<bool> {
        if self.get(&item.id).is_some() {
            return Ok(false);
        }
        self.commit(|board| board.tasks.push(item))?;
        Ok(true)
    }

    /// Drop every Done task, preserving the order of the rest. The removed
    /// tasks are appended to the on-disk archive and returned so the caller
    /// can offer undo. Only persists when something actually changed.
    pub fn clear_done(&mut self) -> io::Result<Vec<TaskItem>> {
        let (done, keep): (Vec<_>, Vec<_>) = self
            .tasks
            .clone()
            .into_iter()
            .partition(|t| t.status == TaskItemStatus::Done);
        if !done.is_empty() {
            archive_tasks(&done)?;
            self.commit(|board| board.tasks = keep)?;
        }
        Ok(done)
    }

    fn commit(&mut self, mutate: impl FnOnce(&mut Self)) -> io::Result<()> {
        let before = self.clone();
        mutate(self);
        self.revision = before.revision.saturating_add(1);
        if let Err(e) = self.save_expected(before.revision) {
            *self = before;
            return Err(e);
        }
        Ok(())
    }

    fn save_expected(&self, expected_revision: u64) -> io::Result<()> {
        let Some(path) = tasks_path() else {
            return Ok(());
        };
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("tasks path has no parent"))?;
        fs::create_dir_all(parent)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(parent.join("tasks.lock"))?;
        lock.lock_exclusive()?;
        let disk_revision = match fs::read_to_string(&path) {
            Ok(raw) => {
                serde_json::from_str::<Self>(&raw)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                    .revision
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e),
        };
        if disk_revision != expected_revision {
            return Err(io::Error::other(
                format!("task board changed on disk (expected revision {expected_revision}, found {disk_revision}); reload and retry"),
            ));
        }
        crate::persist::save_json(&path, self)
    }
}

fn tasks_path() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("tasks.json"))
}

fn archive_path() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("tasks-archive.json"))
}

/// Append removed tasks to `~/.cc-hub/tasks-archive.json` (a bare JSON
/// array), so `x` and `c` are recoverable beyond the in-session undo slot.
/// The archive is a log, not a ledger: an undone delete leaves its copy
/// behind, and a corrupt file starts fresh — same policy as the board.
fn archive_tasks(items: &[TaskItem]) -> io::Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let Some(path) = archive_path() else {
        return Ok(());
    };
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("task archive path has no parent"))?;
    fs::create_dir_all(parent)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(parent.join("tasks-archive.lock"))?;
    lock.lock_exclusive()?;
    let mut archived: Vec<TaskItem> = match fs::read_to_string(&path) {
        Ok(raw) => {
            serde_json::from_str(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    archived.extend(items.iter().cloned());
    crate::persist::save_json(&path, &archived)
}

// Unix-only for the same reason as todo.rs: isolation works by redirecting
// `$HOME`, which `dirs::home_dir()` ignores on Windows.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_util::with_temp_home;

    #[test]
    fn add_persists_round_trip() {
        with_temp_home(|| {
            let mut b = TaskBoard::load();
            assert!(b.tasks().is_empty());
            let id = b.add("  fix the flaky test  ").unwrap().unwrap();
            assert_eq!(b.add("   ").unwrap(), None);
            let reloaded = TaskBoard::load();
            assert_eq!(reloaded.tasks().len(), 1);
            let t = reloaded.get(&id).unwrap();
            assert_eq!(t.text, "fix the flaky test");
            assert_eq!(t.status, TaskItemStatus::Todo);
        });
    }

    #[test]
    fn status_transitions_stamp_done_at() {
        with_temp_home(|| {
            let mut b = TaskBoard::load();
            let id = b.add("ship it").unwrap().unwrap();
            b.set_status(&id, TaskItemStatus::Done).unwrap();
            let t = TaskBoard::load();
            let done = t.get(&id).unwrap();
            assert_eq!(done.status, TaskItemStatus::Done);
            assert!(done.done_at.is_some());
            b.set_status(&id, TaskItemStatus::Todo).unwrap();
            assert!(TaskBoard::load().get(&id).unwrap().done_at.is_none());
        });
    }

    #[test]
    fn assign_moves_to_planning_and_rebind_keeps_session() {
        with_temp_home(|| {
            let mut b = TaskBoard::load();
            let id = b.add("write the parser").unwrap().unwrap();
            b.assign(&id, "/tmp/proj", "claude", "cchub-1-42").unwrap();
            let t = TaskBoard::load();
            let task = t.get(&id).unwrap();
            assert_eq!(task.status, TaskItemStatus::Planning);
            assert_eq!(task.cwd.as_deref(), Some("/tmp/proj"));
            assert_eq!(task.tmux.as_deref(), Some("cchub-1-42"));
            assert_eq!(task.session_id, None);
            b.rebind_tmux(&id, "cchub-1-43").unwrap();
            assert_eq!(
                TaskBoard::load().get(&id).unwrap().tmux.as_deref(),
                Some("cchub-1-43")
            );
        });
    }

    #[test]
    fn assign_records_last_assign_cwd_across_reloads() {
        with_temp_home(|| {
            let mut b = TaskBoard::load();
            assert_eq!(b.last_assign_cwd(), None);
            let id = b.add("t").unwrap().unwrap();
            b.assign(&id, "/tmp/proj", "claude", "cchub-1-42").unwrap();
            assert_eq!(TaskBoard::load().last_assign_cwd(), Some("/tmp/proj"));
            // Deleting the task must not forget where it ran.
            b.remove(&id).unwrap();
            assert_eq!(TaskBoard::load().last_assign_cwd(), Some("/tmp/proj"));
        });
    }

    #[test]
    fn new_tasks_default_priority_and_set_priority_round_trips() {
        with_temp_home(|| {
            let mut b = TaskBoard::load();
            let id = b.add("triage the backlog").unwrap().unwrap();
            // New tasks start at the default priority (P3).
            assert_eq!(b.get(&id).unwrap().priority, TaskPriority::default());
            b.set_priority(&id, TaskPriority::P1).unwrap();
            assert_eq!(
                TaskBoard::load().get(&id).unwrap().priority,
                TaskPriority::P1
            );
            // Re-setting the same level is a no-op (and still persisted state
            // is unchanged).
            b.set_priority(&id, TaskPriority::P1).unwrap();
            assert_eq!(b.get(&id).unwrap().priority, TaskPriority::P1);
        });
    }

    #[test]
    fn missing_priority_field_deserializes_to_default() {
        // A task written by a pre-priority build has no `priority` key; it
        // must load as the default rather than failing the whole board parse.
        let json = r#"{"id":"tk-1","text":"old","status":"todo","created_at":1}"#;
        let t: TaskItem = serde_json::from_str(json).unwrap();
        assert_eq!(t.priority, TaskPriority::P3);
    }

    #[test]
    fn missing_tags_field_deserializes_to_empty() {
        // A pre-tags board has no `tags` key; it must load as an empty vec.
        let json = r#"{"id":"tk-1","text":"old","status":"todo","created_at":1}"#;
        let t: TaskItem = serde_json::from_str(json).unwrap();
        assert!(t.tags.is_empty());
    }

    #[test]
    fn parse_tags_normalizes_and_caps() {
        // Splits on commas and whitespace, strips '#', lowercases, dedupes.
        assert_eq!(
            parse_tags("#Bug,  api   bug BACKEND"),
            vec!["bug", "api", "backend"]
        );
        assert!(parse_tags("   ").is_empty());
        // Per-tag length cap.
        let long = "a".repeat(40);
        assert_eq!(parse_tags(&long), vec!["a".repeat(MAX_TAG_LEN)]);
        // Set-size cap (seven distinct tags → MAX_TAGS kept).
        let many = "t1 t2 t3 t4 t5 t6 t7";
        assert_eq!(parse_tags(many).len(), MAX_TAGS);
    }

    #[test]
    fn set_tags_round_trips_and_skips_unchanged() {
        with_temp_home(|| {
            let mut b = TaskBoard::load();
            let id = b.add("label me").unwrap().unwrap();
            assert!(b.get(&id).unwrap().tags.is_empty());
            b.set_tags(&id, parse_tags("bug api")).unwrap();
            assert_eq!(TaskBoard::load().get(&id).unwrap().tags, vec!["bug", "api"]);
            // Re-setting the same set is a no-op; clearing removes them.
            b.set_tags(&id, parse_tags("bug api")).unwrap();
            assert_eq!(b.get(&id).unwrap().tags, vec!["bug", "api"]);
            b.set_tags(&id, Vec::new()).unwrap();
            assert!(TaskBoard::load().get(&id).unwrap().tags.is_empty());
        });
    }

    #[test]
    fn priority_orders_p1_before_p4() {
        assert!(TaskPriority::P1 < TaskPriority::P2);
        assert!(TaskPriority::P2 < TaskPriority::P3);
        assert!(TaskPriority::P3 < TaskPriority::P4);
    }

    #[test]
    fn column_filters_by_status() {
        with_temp_home(|| {
            let mut b = TaskBoard::load();
            let a = b.add("a").unwrap().unwrap();
            b.add("b").unwrap().unwrap();
            b.set_status(&a, TaskItemStatus::Done).unwrap();
            assert_eq!(b.column(TaskItemStatus::Todo).len(), 1);
            assert_eq!(b.column(TaskItemStatus::Done).len(), 1);
            assert_eq!(b.column(TaskItemStatus::Planning).len(), 0);
            assert_eq!(b.column(TaskItemStatus::InProgress).len(), 0);
        });
    }

    #[test]
    fn clear_done_and_remove() {
        with_temp_home(|| {
            let mut b = TaskBoard::load();
            let a = b.add("a").unwrap().unwrap();
            let c = b.add("c").unwrap().unwrap();
            b.set_status(&a, TaskItemStatus::Done).unwrap();
            let cleared = b.clear_done().unwrap();
            assert_eq!(cleared.len(), 1);
            assert_eq!(cleared[0].id, a);
            assert!(b.clear_done().unwrap().is_empty());
            assert!(b.remove(&c).unwrap().is_some());
            assert!(b.remove(&c).unwrap().is_none());
            assert!(TaskBoard::load().tasks().is_empty());
        });
    }

    #[test]
    fn removals_land_in_archive_and_restore_reinserts() {
        with_temp_home(|| {
            let mut b = TaskBoard::load();
            let a = b.add("deleted one").unwrap().unwrap();
            let d = b.add("done one").unwrap().unwrap();
            b.set_status(&d, TaskItemStatus::Done).unwrap();
            let removed = b.remove(&a).unwrap().unwrap();
            b.clear_done().unwrap();
            // Both removal paths append to the archive file.
            let raw = fs::read_to_string(archive_path().unwrap()).unwrap();
            let archived: Vec<TaskItem> = serde_json::from_str(&raw).unwrap();
            assert_eq!(
                archived.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
                vec![a.as_str(), d.as_str()]
            );
            // Undo restores the task (with its id); a second restore of the
            // same id is refused.
            assert!(b.restore(removed.clone()).unwrap());
            assert!(!b.restore(removed).unwrap());
            assert_eq!(TaskBoard::load().get(&a).unwrap().text, "deleted one");
        });
    }

    #[test]
    fn stale_writer_is_rejected_and_rolled_back() {
        with_temp_home(|| {
            let mut first = TaskBoard::load_result().unwrap();
            let mut stale = TaskBoard::load_result().unwrap();
            first.add("first writer").unwrap().unwrap();

            let error = stale.add("stale writer").unwrap_err();
            assert!(error.to_string().contains("changed on disk"));
            assert!(
                stale.tasks().is_empty(),
                "failed commit must roll memory back"
            );

            let current = TaskBoard::load_result().unwrap();
            assert_eq!(current.tasks().len(), 1);
            assert_eq!(current.tasks()[0].text, "first writer");
        });
    }

    #[test]
    fn malformed_board_is_reported_without_replacing_the_file() {
        with_temp_home(|| {
            let path = tasks_path().unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "{not-json").unwrap();

            let error = TaskBoard::load_result().unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(fs::read_to_string(path).unwrap(), "{not-json");
        });
    }

    #[test]
    fn parse_quick_add_splits_text_tags_priority() {
        let q = parse_quick_add("fix the parser #bug #api !2");
        assert_eq!(q.text, "fix the parser");
        assert_eq!(q.tags, vec!["bug", "api"]);
        assert_eq!(q.priority, Some(TaskPriority::P2));
        // Syntax tokens can sit anywhere; the last `!N` wins; a bare `#`
        // and an out-of-range `!5` stay text.
        let q = parse_quick_add("!4 ship #Infra it !1 # !5");
        assert_eq!(q.text, "ship it # !5");
        assert_eq!(q.tags, vec!["infra"]);
        assert_eq!(q.priority, Some(TaskPriority::P1));
        // Plain input passes through untouched.
        let q = parse_quick_add("just words");
        assert_eq!(q.text, "just words");
        assert!(q.tags.is_empty());
        assert_eq!(q.priority, None);
    }
}
