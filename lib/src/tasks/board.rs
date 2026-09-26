use std::fs;
use std::io;

use super::archive::archive_tasks;
use super::binding::Binding;
use super::meta::{load_board_meta, save_board_meta, BoardMeta};
use super::store::{
    self, read_task_state, task_dir, tasks_dir, update_task, write_task_state, TaskPriority,
    TaskState, TaskStatus,
};

/// In-memory snapshot of the personal board: every `~/.cc-hub/tasks/<id>/`
/// task, ordered by `created_at` (ties broken by id, which embeds nanos).
/// Mutations write through the per-task locked store and update the snapshot
/// from the state the write returned, so what the TUI shows is what landed.
#[derive(Default, Debug)]
pub struct PersonalBoard {
    tasks: Vec<TaskState>,
    last_assign_cwd: Option<String>,
}

impl PersonalBoard {
    /// Load the board from disk. A missing store is an empty board; an
    /// unreadable or malformed task file is an error so callers never
    /// mistake data loss for an intentionally empty board.
    pub fn load_result() -> io::Result<Self> {
        let mut tasks = Vec::new();
        if let Some(dir) = tasks_dir() {
            match fs::read_dir(&dir) {
                Ok(entries) => {
                    for entry in entries {
                        let entry = entry?;
                        if !entry.file_type()?.is_dir() {
                            continue;
                        }
                        let task_id = entry.file_name().to_string_lossy().into_owned();
                        tasks.push(read_task_state(&task_id)?);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        tasks.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.task_id.cmp(&b.task_id))
        });
        Ok(Self {
            tasks,
            last_assign_cwd: load_board_meta().last_assign_cwd,
        })
    }

    /// Convenience for non-interactive callers and tests. Runtime UI code uses
    /// [`Self::load_result`] so it can surface failures.
    pub fn load() -> Self {
        Self::load_result().unwrap_or_default()
    }

    pub fn tasks(&self) -> &[TaskState] {
        &self.tasks
    }

    /// Apply background usage snapshots without replacing newer task edits.
    pub fn update_stats(&mut self, stats: Vec<(String, crate::tasks::stats::TaskStats)>) {
        for (id, stats) in stats {
            if let Some(task) = self.tasks.iter_mut().find(|t| t.task_id == id) {
                task.stats = Some(stats);
            }
        }
    }

    pub fn get(&self, id: &str) -> Option<&TaskState> {
        self.tasks.iter().find(|t| t.task_id == id)
    }

    pub fn last_assign_cwd(&self) -> Option<&str> {
        self.last_assign_cwd.as_deref()
    }

    /// Tasks in `status`, in board order.
    pub fn column(&self, status: TaskStatus) -> Vec<&TaskState> {
        self.tasks.iter().filter(|t| t.status == status).collect()
    }

    /// Append a task (trimmed) to To-Do. Empty/whitespace-only input is
    /// ignored. Returns the new task's id. Persists immediately.
    pub fn add(&mut self, text: &str) -> io::Result<Option<String>> {
        self.add_configured(text, Vec::new(), TaskPriority::default())
    }

    /// Add a task and its quick-capture metadata in one write, so a failure
    /// cannot leave behind a card missing its tags/priority.
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
        let mut state = TaskState::new(text.to_string());
        state.tags = tags;
        state.priority = priority;
        write_task_state(&state)?;
        let id = state.task_id.clone();
        self.tasks.push(state);
        Ok(Some(id))
    }

    /// Replace a task's text (trimmed), leaving status and any agent
    /// binding untouched. Empty/whitespace-only input and unknown ids are
    /// ignored (returns false). Persists.
    pub fn rename(&mut self, id: &str, text: &str) -> io::Result<bool> {
        let text = text.trim();
        if text.is_empty() || self.get(id).is_none_or(|t| t.prompt == text) {
            return Ok(false);
        }
        let updated = update_task(id, |s| s.prompt = text.to_string())?;
        self.adopt(updated);
        Ok(true)
    }

    /// Set a task's priority. Skips the disk write when the priority is
    /// unchanged (re-pressing the same level is a no-op). No-op on unknown
    /// id. Persists when it changes.
    pub fn set_priority(&mut self, id: &str, priority: TaskPriority) -> io::Result<bool> {
        if self.get(id).is_none_or(|t| t.priority == priority) {
            return Ok(false);
        }
        let updated = update_task(id, |s| s.priority = priority)?;
        self.adopt(updated);
        Ok(true)
    }

    /// Replace a task's tags with `tags` (already normalized by
    /// [`super::parse_tags`]). Skips the disk write when unchanged. No-op on unknown
    /// id. Persists when it changes.
    pub fn set_tags(&mut self, id: &str, tags: Vec<String>) -> io::Result<bool> {
        if self.get(id).is_none_or(|t| t.tags == tags) {
            return Ok(false);
        }
        let updated = update_task(id, |s| s.tags = tags)?;
        self.adopt(updated);
        Ok(true)
    }

    /// Set (or with `None` clear) a task's deliverable kind — the word the
    /// task router places the card by. Skips the disk write when unchanged.
    /// No-op on unknown id. Persists when it changes.
    pub fn set_kind(&mut self, id: &str, kind: Option<String>) -> io::Result<bool> {
        if self.get(id).is_none_or(|t| t.kind == kind) {
            return Ok(false);
        }
        let updated = update_task(id, |s| s.kind = kind)?;
        self.adopt(updated);
        Ok(true)
    }

    /// Move a task between columns, stamping/clearing `done_at` so the Done
    /// column can show when it landed. No-op on unknown id. Persists. The
    /// shared transition table validates the edge, so an illegal move (e.g.
    /// from a hand-edited state file) errors instead of silently landing.
    pub fn set_status(&mut self, id: &str, status: TaskStatus) -> io::Result<bool> {
        if self.get(id).is_none_or(|t| t.status == status) {
            return Ok(false);
        }
        let updated = update_task(id, |s| {
            s.status = status;
            s.done_at = (status == TaskStatus::Done).then(store::now_unix_secs);
        })?;
        self.adopt(updated);
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
        let updated = update_task(id, |s| {
            s.cwd = Some(cwd.to_string());
            s.agent_id = Some(agent_id.to_string());
            s.tmux = Some(tmux.to_string());
            // A re-assign spawns a fresh session; the old session id no
            // longer matches the new tmux, so drop it until the next scan
            // re-resolves.
            s.session_id = None;
            s.status = TaskStatus::Planning;
            s.done_at = None;
        })?;
        self.adopt(updated);
        self.last_assign_cwd = Some(cwd.to_string());
        save_board_meta(&BoardMeta {
            last_assign_cwd: self.last_assign_cwd.clone(),
        })?;
        Ok(true)
    }

    /// Point an existing assignment at a new mux session (resume after the
    /// old tmux died). Keeps `session_id` — resume continues that session.
    pub fn rebind_tmux(&mut self, id: &str, tmux: &str) -> io::Result<bool> {
        if self.get(id).is_none_or(|t| t.tmux.as_deref() == Some(tmux)) {
            return Ok(false);
        }
        let updated = update_task(id, |s| s.tmux = Some(tmux.to_string()))?;
        self.adopt(updated);
        Ok(true)
    }

    /// A managed role changed account. Preserve the user's board status.
    pub fn bind_resource(
        &mut self,
        id: &str,
        cwd: &str,
        agent: &str,
        tmux: &str,
        sid: Option<&str>,
    ) -> io::Result<bool> {
        if self.get(id).is_none() {
            return Ok(false);
        }
        let updated = update_task(id, |s| {
            s.cwd = Some(cwd.into());
            s.agent_id = Some(agent.into());
            s.tmux = Some(tmux.into());
            s.session_id = sid.map(str::to_string);
        })?;
        self.adopt(updated);
        Ok(true)
    }

    /// Keep every card's binding — `session_id` and `tmux` — in step with the
    /// scan, so `f`, Space and Done act on one session, not on whichever field
    /// was written last. The session the card names wins: if it is live, the
    /// card follows it to wherever it runs now (a replacement on another
    /// account, a hand-over). Otherwise the card learns the session running in
    /// its tmux — a fresh assignment, or a Claude resume that forked a new id.
    /// Returns true when something was learned (and persisted) so callers can
    /// repaint.
    pub fn bind_sessions(&mut self, sessions: &[crate::models::SessionInfo]) -> io::Result<bool> {
        let bindings: Vec<(String, Binding)> = self
            .tasks
            .iter()
            .filter_map(|t| Binding::learned(t, sessions).map(|b| (t.task_id.clone(), b)))
            .collect();
        if bindings.is_empty() {
            return Ok(false);
        }
        for (id, binding) in bindings {
            let updated = update_task(&id, |s| binding.apply(s))?;
            self.adopt(updated);
        }
        Ok(true)
    }

    /// Remove the task with `id`, returning it so the caller can describe
    /// what was deleted (and whether an agent session survives it). The
    /// removed task is appended to the on-disk archive. Persists.
    pub fn remove(&mut self, id: &str) -> io::Result<Option<TaskState>> {
        let Some(idx) = self.tasks.iter().position(|t| t.task_id == id) else {
            return Ok(None);
        };
        let removed = self.tasks[idx].clone();
        // Archive first: an extra archive entry is harmless, while removing
        // the task dir before a failed archive write would report failure
        // after the card had already disappeared.
        archive_tasks(std::slice::from_ref(&removed))?;
        delete_task_dir(id)?;
        self.tasks.remove(idx);
        Ok(Some(removed))
    }

    /// Re-insert a previously removed task (undo of `x`/`c`). Skips ids
    /// already on the board (double-undo, hand edits); returns whether it
    /// landed. Persists.
    pub fn restore(&mut self, item: TaskState) -> io::Result<bool> {
        if self.get(&item.task_id).is_some() {
            return Ok(false);
        }
        write_task_state(&item)?;
        self.tasks.push(item);
        Ok(true)
    }

    /// Drop every Done task, preserving the order of the rest. The removed
    /// tasks are appended to the on-disk archive and returned so the caller
    /// can offer undo. Only persists when something actually changed.
    pub fn clear_done(&mut self) -> io::Result<Vec<TaskState>> {
        let done: Vec<TaskState> = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Done)
            .cloned()
            .collect();
        if done.is_empty() {
            return Ok(done);
        }
        archive_tasks(&done)?;
        for t in &done {
            delete_task_dir(&t.task_id)?;
        }
        self.tasks.retain(|t| t.status != TaskStatus::Done);
        Ok(done)
    }

    /// Replace the in-memory copy of a task with the state a locked write
    /// returned, so the snapshot shows what landed without a full reload.
    /// Ops that write outside the board (the artifact ops in `ops::task`)
    /// hand their result back through here too.
    pub(crate) fn adopt(&mut self, updated: TaskState) {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.task_id == updated.task_id) {
            *t = updated;
        }
    }
}

/// Delete a task's directory. Already gone counts as deleted.
fn delete_task_dir(id: &str) -> io::Result<()> {
    if let Some(dir) = task_dir(id) {
        match fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

// Unix-only for the same reason as todo.rs: isolation works by redirecting
// `$HOME`, which `dirs::home_dir()` ignores on Windows.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::tasks::parse_tags;
    use crate::test_util::with_temp_home;

    #[test]
    fn add_persists_round_trip() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            assert!(b.tasks().is_empty());
            let id = b.add("  fix the flaky test  ").unwrap().unwrap();
            assert_eq!(b.add("   ").unwrap(), None);
            let reloaded = PersonalBoard::load();
            assert_eq!(reloaded.tasks().len(), 1);
            let t = reloaded.get(&id).unwrap();
            assert_eq!(t.prompt, "fix the flaky test");
            assert_eq!(t.status, TaskStatus::Backlog);
            assert!(t.task_id.starts_with("tk-"));
        });
    }

    #[test]
    fn status_transitions_stamp_done_at() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            let id = b.add("ship it").unwrap().unwrap();
            b.set_status(&id, TaskStatus::Done).unwrap();
            let t = PersonalBoard::load();
            let done = t.get(&id).unwrap();
            assert_eq!(done.status, TaskStatus::Done);
            assert!(done.done_at.is_some());
            b.set_status(&id, TaskStatus::Backlog).unwrap();
            assert!(PersonalBoard::load().get(&id).unwrap().done_at.is_none());
        });
    }

    #[test]
    fn illegal_edge_is_rejected_and_not_persisted() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            let id = b.add("no PR flow here").unwrap().unwrap();
            // Backlog → Review skips the work; the transition table must
            // refuse it.
            let err = b.set_status(&id, TaskStatus::Review).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(
                PersonalBoard::load().get(&id).unwrap().status,
                TaskStatus::Backlog
            );
            // The rejected write must not poison the in-memory snapshot.
            assert_eq!(b.get(&id).unwrap().status, TaskStatus::Backlog);
        });
    }

    #[test]
    fn assign_moves_to_planning_and_rebind_keeps_session() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            let id = b.add("write the parser").unwrap().unwrap();
            b.assign(&id, "/tmp/proj", "claude", "cchub-1-42").unwrap();
            let t = PersonalBoard::load();
            let task = t.get(&id).unwrap();
            assert_eq!(task.status, TaskStatus::Planning);
            assert_eq!(task.cwd.as_deref(), Some("/tmp/proj"));
            assert_eq!(task.tmux.as_deref(), Some("cchub-1-42"));
            assert_eq!(task.session_id, None);
            b.rebind_tmux(&id, "cchub-1-43").unwrap();
            assert_eq!(
                PersonalBoard::load().get(&id).unwrap().tmux.as_deref(),
                Some("cchub-1-43")
            );
        });
    }

    #[test]
    fn new_tasks_default_priority_and_set_priority_round_trips() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            let id = b.add("triage the backlog").unwrap().unwrap();
            // New tasks start at the default priority (P3).
            assert_eq!(b.get(&id).unwrap().priority, TaskPriority::default());
            b.set_priority(&id, TaskPriority::P1).unwrap();
            assert_eq!(
                PersonalBoard::load().get(&id).unwrap().priority,
                TaskPriority::P1
            );
            // Re-setting the same level is a no-op (and still persisted state
            // is unchanged).
            b.set_priority(&id, TaskPriority::P1).unwrap();
            assert_eq!(b.get(&id).unwrap().priority, TaskPriority::P1);
        });
    }

    #[test]
    fn set_kind_round_trips_and_clears() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            let id = b.add("route me").unwrap().unwrap();
            assert!(
                b.get(&id).unwrap().kind.is_none(),
                "router-chosen by default"
            );
            assert!(b.set_kind(&id, Some("ai-plugin".into())).unwrap());
            assert_eq!(
                PersonalBoard::load().get(&id).unwrap().kind.as_deref(),
                Some("ai-plugin")
            );
            // Re-picking the same kind writes nothing; the clear row does.
            assert!(!b.set_kind(&id, Some("ai-plugin".into())).unwrap());
            assert!(b.set_kind(&id, None).unwrap());
            assert!(PersonalBoard::load().get(&id).unwrap().kind.is_none());
        });
    }

    #[test]
    fn set_tags_round_trips_and_skips_unchanged() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            let id = b.add("label me").unwrap().unwrap();
            assert!(b.get(&id).unwrap().tags.is_empty());
            b.set_tags(&id, parse_tags("bug api")).unwrap();
            assert_eq!(
                PersonalBoard::load().get(&id).unwrap().tags,
                vec!["bug", "api"]
            );
            // Re-setting the same set is a no-op; clearing removes them.
            b.set_tags(&id, parse_tags("bug api")).unwrap();
            assert_eq!(b.get(&id).unwrap().tags, vec!["bug", "api"]);
            b.set_tags(&id, Vec::new()).unwrap();
            assert!(PersonalBoard::load().get(&id).unwrap().tags.is_empty());
        });
    }

    #[test]
    fn column_filters_by_status() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            let a = b.add("a").unwrap().unwrap();
            b.add("b").unwrap().unwrap();
            b.set_status(&a, TaskStatus::Done).unwrap();
            assert_eq!(b.column(TaskStatus::Backlog).len(), 1);
            assert_eq!(b.column(TaskStatus::Done).len(), 1);
            assert_eq!(b.column(TaskStatus::Planning).len(), 0);
            assert_eq!(b.column(TaskStatus::Running).len(), 0);
        });
    }

    #[test]
    fn clear_done_and_remove() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            let a = b.add("a").unwrap().unwrap();
            let c = b.add("c").unwrap().unwrap();
            b.set_status(&a, TaskStatus::Done).unwrap();
            let cleared = b.clear_done().unwrap();
            assert_eq!(cleared.len(), 1);
            assert_eq!(cleared[0].task_id, a);
            assert!(b.clear_done().unwrap().is_empty());
            assert!(b.remove(&c).unwrap().is_some());
            assert!(b.remove(&c).unwrap().is_none());
            assert!(PersonalBoard::load().tasks().is_empty());
        });
    }

    #[test]
    fn concurrent_instances_merge_at_task_granularity() {
        with_temp_home(|| {
            // Two boards loaded from the same (empty) store: each adds a
            // task; both must land on disk. The old single-file board's CAS
            // would have rejected the second writer entirely.
            let mut first = PersonalBoard::load_result().unwrap();
            let mut second = PersonalBoard::load_result().unwrap();
            first.add("first writer").unwrap().unwrap();
            second.add("second writer").unwrap().unwrap();

            let merged = PersonalBoard::load_result().unwrap();
            assert_eq!(merged.tasks().len(), 2);
        });
    }

    #[test]
    fn malformed_task_file_is_reported_without_replacing_it() {
        with_temp_home(|| {
            let dir = task_dir("tk-broken").unwrap();
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("state.json"), "{not-json").unwrap();

            let error = PersonalBoard::load_result().unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(
                fs::read_to_string(dir.join("state.json")).unwrap(),
                "{not-json"
            );
        });
    }

    #[test]
    fn a_card_changing_column_wakes_the_board() {
        with_temp_home(|| {
            let wake = crate::wake::Wake::named(crate::wake::BOARD).unwrap();
            let mut b = PersonalBoard::load();
            let id = b.add("route me").unwrap().unwrap();
            let minted = wake.last();

            // Only a column change is news: renaming a card is not.
            b.rename(&id, "route me now").unwrap();
            assert_eq!(wake.last(), minted, "a rename is not a move");

            b.set_status(&id, TaskStatus::Planning).unwrap();
            let planning = wake.last();
            assert!(planning.is_some());
            assert_ne!(planning, minted, "Backlog → Planning is a move");

            b.set_status(&id, TaskStatus::Planning).unwrap();
            assert_eq!(wake.last(), planning, "a no-op is not a move");

            b.set_status(&id, TaskStatus::Running).unwrap();
            assert_ne!(wake.last(), planning, "Planning → Running is a move");
        });
    }
}
