use crate::projects_scan::ProjectsSnapshot;

/// Projects-tab state: the scanned snapshot plus every cursor/flag the
/// Projects tab and its overlays (kanban, Backlog popup, Result popup,
/// project-creation flow) navigate with. Grouped onto its own struct so the
/// kanban/backlog cursors can only be moved through the clamping methods.
pub struct ProjectsView {
    /// Latest scan snapshot; drives the kanban columns and chip strip.
    pub snapshot: ProjectsSnapshot,
    /// Cursor in the Projects tab. `0..snapshot.projects.len()` selects a
    /// project; task selection within the project lives in [`Self::task_sel`].
    pub sel: usize,
    pub task_sel: usize,
    /// Kanban column cursor: 0=Planning, 1=Running, 2=Review, 3=Merging,
    /// 4=Done. Drives which column [`Self::task_sel`] indexes into.
    pub col: usize,
    /// Cursor inside the Backlog popup, indexing into the selected
    /// project's backlog tasks. Reset on popup open. Backlog tasks live
    /// off the kanban (which starts at Planning) so this popup is the
    /// only way to see and start them.
    pub backlog_sel: usize,
    /// True while the folder picker / prompt input flow is creating a
    /// new project task (vs. spawning a regular session). Used to route
    /// the picker's space-pick and prompt-input's enter to the project
    /// flow instead of [`spawn::spawn_claude_session`].
    pub creating_task: bool,
    /// True while the folder picker is in "register a project, no task"
    /// mode (the `N` shortcut from the Projects view). Picking a folder
    /// just runs [`crate::orchestrator::ensure_project_registered`] and
    /// closes the picker — no orchestrator is spawned.
    pub registering_only: bool,
    /// cwd captured when the picker chose a folder in Projects mode. Held
    /// until the user submits the task prompt; consumed in
    /// [`App::submit_project_task`].
    pub pending_cwd: Option<String>,
    /// Agent override for the pending project task. `None` means "use the
    /// project default at spawn time"; `Some(id)` is set by the user
    /// cycling backends in the prompt-input view via Tab.
    pub pending_agent_id: Option<String>,
    /// Task id we want the kanban cursor to jump to once the next
    /// ProjectsSnapshot includes it. Set when the user starts a Backlog
    /// task; cleared in update_projects once focus has moved (or when
    /// the budget below runs out).
    pub pending_focus_task_id: Option<String>,
    /// Snapshot ticks remaining to find pending_focus_task_id before we
    /// give up with a soft toast. Started at 5 — the fs-watcher-driven
    /// scan typically lands within one tick, but the periodic 2s ticker
    /// can interleave, so allow a few attempts.
    pub pending_focus_budget: u8,
    /// Cursor inside the Projects "Result" popup, indexing into the
    /// selected task's `artifacts` vec. Reset on popup open.
    pub result_artifact_sel: usize,
}

impl ProjectsView {
    pub(crate) fn new() -> Self {
        Self {
            snapshot: ProjectsSnapshot::empty(),
            sel: 0,
            task_sel: 0,
            col: 0,
            backlog_sel: 0,
            creating_task: false,
            registering_only: false,
            pending_cwd: None,
            pending_agent_id: None,
            pending_focus_task_id: None,
            pending_focus_budget: 0,
            result_artifact_sel: 0,
        }
    }

    pub fn selected_project(&self) -> Option<&crate::orchestrator::Project> {
        self.snapshot.projects.get(self.sel)
    }

    /// Tasks in the currently-selected project that match the given
    /// kanban column. Columns are derived from `TaskStatus` + worker
    /// presence: a Running task with no workers is in "Planning"
    /// (orchestrator is still decomposing); Running + workers is true
    /// Running; Review/Merging/Done map straight from status.
    ///
    /// Indices: 0=Planning, 1=Running, 2=Review, 3=Merging, 4=Done.
    /// Order matches the underlying `tasks` Vec (already sorted
    /// newest-first by the orchestrator).
    pub fn kanban_column_tasks(&self, col: usize) -> Vec<&crate::orchestrator::TaskState> {
        let Some(p) = self.selected_project() else {
            return Vec::new();
        };
        let Some(tasks) = self.snapshot.tasks.get(&p.id) else {
            return Vec::new();
        };
        use crate::orchestrator::TaskStatus;
        tasks
            .iter()
            .filter(|t| match col {
                0 => t.status == TaskStatus::Running && t.workers.is_empty(),
                1 => t.status == TaskStatus::Running && !t.workers.is_empty(),
                2 => t.status == TaskStatus::Review,
                3 => t.status == TaskStatus::Merging,
                _ => t.status == TaskStatus::Done,
            })
            .map(|t| t.as_ref())
            .collect()
    }

    pub fn kanban_column_len(&self, col: usize) -> usize {
        self.kanban_column_tasks(col).len()
    }

    /// Backlog tasks for the currently-selected project, in scan order
    /// (newest first, same as the underlying `tasks` Vec). Backlog tasks
    /// don't appear in the kanban; the Backlog popup (`View::Backlog`) is
    /// where they're listed and started.
    pub fn backlog_tasks(&self) -> Vec<&crate::orchestrator::TaskState> {
        let Some(p) = self.selected_project() else {
            return Vec::new();
        };
        let Some(tasks) = self.snapshot.tasks.get(&p.id) else {
            return Vec::new();
        };
        use crate::orchestrator::TaskStatus;
        tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Backlog)
            .map(|t| t.as_ref())
            .collect()
    }

    pub fn selected_backlog_task(&self) -> Option<&crate::orchestrator::TaskState> {
        self.backlog_tasks().get(self.backlog_sel).copied()
    }

    pub fn selected_project_task(&self) -> Option<&crate::orchestrator::TaskState> {
        let col = self.kanban_column_tasks(self.col);
        col.get(self.task_sel).copied()
    }

    /// Search the focused project's kanban columns for `task_id`. If found,
    /// move `col` / `task_sel` onto it and return the column index. Returns
    /// None if not found in any column (or if no project is selected).
    pub fn focus_task(&mut self, task_id: &str) -> Option<usize> {
        for col in 0..5 {
            let tasks = self.kanban_column_tasks(col);
            if let Some(row) = tasks.iter().position(|t| t.task_id == task_id) {
                self.col = col;
                self.task_sel = row;
                return Some(col);
            }
        }
        None
    }

    pub(crate) fn clamp_cursor(&mut self) {
        self.clamp_cursor_inner(false);
    }

    /// Like [`Self::clamp_cursor`] but, if the focused column ends up empty,
    /// jumps to the first non-empty column. Use at user-driven entry points
    /// (project switch, approve_review) — not on every rescan, since that
    /// would override an explicit column selection on the next tick.
    pub(crate) fn clamp_cursor_jump_if_empty(&mut self) {
        self.clamp_cursor_inner(true);
    }

    fn clamp_cursor_inner(&mut self, jump_if_empty: bool) {
        let n = self.snapshot.projects.len();
        if n == 0 {
            self.sel = 0;
            self.task_sel = 0;
            self.col = 0;
            return;
        }
        if self.sel >= n {
            self.sel = n - 1;
        }
        if self.col > 4 {
            self.col = 4;
        }
        if jump_if_empty && self.kanban_column_len(self.col) == 0 {
            for col in 0..5 {
                if self.kanban_column_len(col) > 0 {
                    self.col = col;
                    break;
                }
            }
        }
        let col_count = self.kanban_column_len(self.col);
        if col_count == 0 {
            self.task_sel = 0;
        } else if self.task_sel >= col_count {
            self.task_sel = col_count - 1;
        }
    }

    /// Cycle through projects (top chip strip), wrapping at the ends. Clamp
    /// the task cursor against the newly-focused project immediately so the
    /// kanban never lands on an out-of-range row or an empty column with a
    /// non-empty neighbor.
    pub fn move_down(&mut self) {
        let n = self.snapshot.projects.len();
        if n == 0 {
            return;
        }
        self.sel = (self.sel + 1) % n;
        self.clamp_cursor_jump_if_empty();
    }

    pub fn move_up(&mut self) {
        let n = self.snapshot.projects.len();
        if n == 0 {
            return;
        }
        self.sel = if self.sel == 0 { n - 1 } else { self.sel - 1 };
        self.clamp_cursor_jump_if_empty();
    }

    /// Move cursor down within the current kanban column.
    pub fn task_next(&mut self) {
        let col_count = self.kanban_column_len(self.col);
        if col_count == 0 {
            return;
        }
        self.task_sel = (self.task_sel + 1).min(col_count - 1);
    }

    pub fn task_prev(&mut self) {
        self.task_sel = self.task_sel.saturating_sub(1);
    }

    /// Move kanban cursor one column right (Planning → Running → Review
    /// → Merging → Done). Clamps the row cursor to the destination
    /// column's length so the selection is preserved where possible
    /// instead of snapping back to the top card.
    pub fn col_right(&mut self) {
        if self.col < 4 {
            self.col += 1;
            self.clamp_cursor();
        }
    }

    /// Move kanban cursor one column left. Clamps the row cursor to the
    /// destination column's length rather than resetting it to the top.
    pub fn col_left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            self.clamp_cursor();
        }
    }

    /// Reset the chip-strip cursor to the first project. Used after a
    /// project removal where the selection may dangle past the now-removed
    /// project until the next scan tick lands.
    pub fn reset_project_cursor(&mut self) {
        self.sel = 0;
    }

    /// Request that the kanban cursor jump to `task_id` once the next
    /// ProjectsSnapshot includes it. Started/restarted tasks call this so
    /// focus follows the new card into whichever column it lands in; the
    /// jump (and budget countdown) is driven by [`App::update_projects`].
    pub fn request_focus(&mut self, task_id: String) {
        self.pending_focus_task_id = Some(task_id);
        self.pending_focus_budget = 5;
    }

    pub fn backlog_up(&mut self) {
        if self.backlog_sel > 0 {
            self.backlog_sel -= 1;
        }
    }

    pub fn backlog_down(&mut self) {
        let n = self.backlog_tasks().len();
        if n > 0 && self.backlog_sel + 1 < n {
            self.backlog_sel += 1;
        }
    }

    /// Clamp the Backlog cursor to the current backlog length. Used after a
    /// backlog-task delete where the model may still include the removed
    /// task until the next scan tick.
    pub fn clamp_backlog_cursor(&mut self) {
        self.backlog_sel = self
            .backlog_sel
            .min(self.backlog_tasks().len().saturating_sub(1));
    }

    pub fn result_artifact_next(&mut self) {
        let n = self
            .selected_project_task()
            .map(|t| t.artifacts.len())
            .unwrap_or(0);
        if n == 0 {
            self.result_artifact_sel = 0;
            return;
        }
        self.result_artifact_sel = (self.result_artifact_sel + 1).min(n - 1);
    }

    pub fn result_artifact_prev(&mut self) {
        self.result_artifact_sel = self.result_artifact_sel.saturating_sub(1);
    }

    /// The artifact under the popup cursor, if any. Used by the `c` and `o`
    /// keybinds to know what path to act on.
    pub fn selected_result_artifact(&self) -> Option<&crate::orchestrator::Artifact> {
        let t = self.selected_project_task()?;
        t.artifacts.get(self.result_artifact_sel)
    }
}
