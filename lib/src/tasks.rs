//! Personal task board shown on the Tasks tab. Unlike the Projects-tab
//! orchestrator tasks (heavyweight: orchestrator session, workers, PR
//! pipeline), a board task is a plain to-do item that can optionally be
//! handed to a single agent session: assigning spawns a detached agent in a
//! chosen cwd with the task text as its prompt and records the binding, so
//! `f` on the card attaches to that session exactly like the Sessions tab.
//! Stored at `~/.cc-hub/tasks.json` as an ordered array so the file is
//! trivially editable by hand if needed.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::platform::paths::cc_hub_home;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskItemStatus {
    Todo,
    InProgress,
    Done,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskItem {
    pub id: String,
    pub text: String,
    pub status: TaskItemStatus,
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
    /// Load the board from disk. A missing or unparseable file yields an
    /// empty board — same recover-by-starting-fresh policy as the to-do
    /// scratchpad, since the board is a convenience, not a ledger.
    pub fn load() -> Self {
        let Some(path) = tasks_path() else {
            return Self::default();
        };
        let Ok(raw) = fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    pub fn tasks(&self) -> &[TaskItem] {
        &self.tasks
    }

    pub fn get(&self, id: &str) -> Option<&TaskItem> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// Tasks in `status`, in insertion order.
    pub fn column(&self, status: TaskItemStatus) -> Vec<&TaskItem> {
        self.tasks.iter().filter(|t| t.status == status).collect()
    }

    /// Append a task (trimmed) to To-Do. Empty/whitespace-only input is
    /// ignored. Returns the new task's id. Persists immediately.
    pub fn add(&mut self, text: &str) -> Option<String> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let id = new_task_item_id();
        self.tasks.push(TaskItem {
            id: id.clone(),
            text: text.to_string(),
            status: TaskItemStatus::Todo,
            created_at: now_secs(),
            done_at: None,
            cwd: None,
            agent_id: None,
            tmux: None,
            session_id: None,
        });
        let _ = self.save();
        Some(id)
    }

    /// Move a task between columns, stamping/clearing `done_at` so the Done
    /// column can show when it landed. No-op on unknown id. Persists.
    pub fn set_status(&mut self, id: &str, status: TaskItemStatus) {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
            t.status = status;
            t.done_at = (status == TaskItemStatus::Done).then(now_secs);
            let _ = self.save();
        }
    }

    /// Record an agent assignment: where it runs, which agent, and the mux
    /// session it lives in. Moves the task to In Progress. Persists.
    pub fn assign(&mut self, id: &str, cwd: &str, agent_id: &str, tmux: &str) {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
            t.cwd = Some(cwd.to_string());
            t.agent_id = Some(agent_id.to_string());
            t.tmux = Some(tmux.to_string());
            // A re-assign spawns a fresh session; the old session id no
            // longer matches the new tmux, so drop it until the next scan
            // re-resolves.
            t.session_id = None;
            t.status = TaskItemStatus::InProgress;
            t.done_at = None;
            let _ = self.save();
        }
    }

    /// Point an existing assignment at a new mux session (resume after the
    /// old tmux died). Keeps `session_id` — resume continues that session.
    pub fn rebind_tmux(&mut self, id: &str, tmux: &str) {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
            t.tmux = Some(tmux.to_string());
            let _ = self.save();
        }
    }

    /// Fill in `session_id` for any assigned task whose tmux name matches a
    /// scanned session. Returns true when something new was learned (and
    /// persisted) so callers can repaint.
    pub fn bind_sessions(&mut self, sessions: &[crate::models::SessionInfo]) -> bool {
        let mut changed = false;
        for t in &mut self.tasks {
            if t.session_id.is_some() {
                continue;
            }
            let Some(tmux) = t.tmux.as_deref() else {
                continue;
            };
            if let Some(s) = sessions.iter().find(|s| s.tmux_session.as_deref() == Some(tmux)) {
                t.session_id = Some(s.session_id.clone());
                changed = true;
            }
        }
        if changed {
            let _ = self.save();
        }
        changed
    }

    /// Remove the task with `id`, returning it so the caller can describe
    /// what was deleted (and whether an agent session survives it). Persists.
    pub fn remove(&mut self, id: &str) -> Option<TaskItem> {
        let idx = self.tasks.iter().position(|t| t.id == id)?;
        let removed = self.tasks.remove(idx);
        let _ = self.save();
        Some(removed)
    }

    /// Drop every Done task, preserving the order of the rest. Returns the
    /// number removed. Only persists when something actually changed.
    pub fn clear_done(&mut self) -> usize {
        let before = self.tasks.len();
        self.tasks.retain(|t| t.status != TaskItemStatus::Done);
        let removed = before - self.tasks.len();
        if removed > 0 {
            let _ = self.save();
        }
        removed
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = tasks_path() else {
            return Ok(());
        };
        crate::persist::save_json(&path, self)
    }
}

fn tasks_path() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("tasks.json"))
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
            let id = b.add("  fix the flaky test  ").unwrap();
            assert_eq!(b.add("   "), None);
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
            let id = b.add("ship it").unwrap();
            b.set_status(&id, TaskItemStatus::Done);
            let t = TaskBoard::load();
            let done = t.get(&id).unwrap();
            assert_eq!(done.status, TaskItemStatus::Done);
            assert!(done.done_at.is_some());
            b.set_status(&id, TaskItemStatus::Todo);
            assert!(TaskBoard::load().get(&id).unwrap().done_at.is_none());
        });
    }

    #[test]
    fn assign_moves_to_in_progress_and_rebind_keeps_session() {
        with_temp_home(|| {
            let mut b = TaskBoard::load();
            let id = b.add("write the parser").unwrap();
            b.assign(&id, "/tmp/proj", "claude", "cchub-1-42");
            let t = TaskBoard::load();
            let task = t.get(&id).unwrap();
            assert_eq!(task.status, TaskItemStatus::InProgress);
            assert_eq!(task.cwd.as_deref(), Some("/tmp/proj"));
            assert_eq!(task.tmux.as_deref(), Some("cchub-1-42"));
            assert_eq!(task.session_id, None);
            b.rebind_tmux(&id, "cchub-1-43");
            assert_eq!(
                TaskBoard::load().get(&id).unwrap().tmux.as_deref(),
                Some("cchub-1-43")
            );
        });
    }

    #[test]
    fn column_filters_by_status() {
        with_temp_home(|| {
            let mut b = TaskBoard::load();
            let a = b.add("a").unwrap();
            b.add("b").unwrap();
            b.set_status(&a, TaskItemStatus::Done);
            assert_eq!(b.column(TaskItemStatus::Todo).len(), 1);
            assert_eq!(b.column(TaskItemStatus::Done).len(), 1);
            assert_eq!(b.column(TaskItemStatus::InProgress).len(), 0);
        });
    }

    #[test]
    fn clear_done_and_remove() {
        with_temp_home(|| {
            let mut b = TaskBoard::load();
            let a = b.add("a").unwrap();
            let c = b.add("c").unwrap();
            b.set_status(&a, TaskItemStatus::Done);
            assert_eq!(b.clear_done(), 1);
            assert_eq!(b.clear_done(), 0);
            assert!(b.remove(&c).is_some());
            assert!(b.remove(&c).is_none());
            assert!(TaskBoard::load().tasks().is_empty());
        });
    }
}
