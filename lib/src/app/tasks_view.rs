//! Per-tab state for the Tasks tab: the persistent board plus the kanban
//! cursor (column, row), the add-task input buffer, and the id of a task
//! waiting on a folder pick for agent assignment.

use crate::config;
use crate::fuzzy::fuzzy_match;
use crate::orchestrator::{TaskState, TaskStatus};
use crate::tasks::PersonalBoard;

/// Full board column order. Planning is optional at render time (see
/// [`visible_task_columns`]); this stays the canonical set the board logic
/// and on-disk statuses are defined against.
pub const TASK_COLUMNS: [TaskStatus; 4] = [
    TaskStatus::Backlog,
    TaskStatus::Planning,
    TaskStatus::Running,
    TaskStatus::Done,
];

/// Columns shown on the Tasks board, in cursor (`col`) order. The Planning
/// column is optional (`ui.show_planning_column`); when off it's dropped here
/// and its cards fold into In Progress (see [`column_statuses`]), so the
/// status stays reachable — only the dedicated column disappears.
pub fn visible_task_columns() -> Vec<TaskStatus> {
    visible_columns(config::get().ui.show_planning_column)
}

/// The board statuses one visible column renders. One status per column,
/// except In Progress absorbs Planning when the Planning column is hidden so
/// plan-ready cards still appear (and Space still approves them — the action
/// keys off the card's own status).
pub fn column_statuses(col: TaskStatus) -> Vec<TaskStatus> {
    statuses_for(col, config::get().ui.show_planning_column)
}

/// Pure core of [`visible_task_columns`], split out so the column logic is
/// testable without the global config singleton.
fn visible_columns(show_planning: bool) -> Vec<TaskStatus> {
    TASK_COLUMNS
        .into_iter()
        .filter(|s| show_planning || *s != TaskStatus::Planning)
        .collect()
}

/// Pure core of [`column_statuses`].
fn statuses_for(col: TaskStatus, show_planning: bool) -> Vec<TaskStatus> {
    if col == TaskStatus::Running && !show_planning {
        vec![TaskStatus::Planning, TaskStatus::Running]
    } else {
        vec![col]
    }
}

/// Which field of the add-task popup keystrokes land in. The popup is two
/// fields — the one-line task text and the free-form context under it — and
/// Tab moves between them. Rename mode only ever uses [`TaskField::Text`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TaskField {
    #[default]
    Text,
    Context,
}

pub struct TasksView {
    pub board: PersonalBoard,
    /// Most recent repository failure, consumed by `App` and shown in the
    /// status bar rather than allowing a failed write/load to look successful.
    pub persistence_error: Option<String>,
    /// Focused column (index into [`TASK_COLUMNS`]).
    pub col: usize,
    /// Row cursor within the focused column.
    pub row: usize,
    /// In-progress text for the add/rename-task popup
    /// ([`crate::app::View::TaskInput`]).
    pub input: String,
    /// Free-form context typed or pasted under the task line in the add
    /// popup. Committed as the new task's first `note` attachment, so the
    /// agent that later picks the card up reads it (see
    /// [`crate::app::App::submit_task_input`]). Unused while renaming.
    pub context: String,
    /// Which of the two add-popup fields keystrokes land in.
    pub field: TaskField,
    /// Task id being renamed: set when `r` opens the popup, consumed on
    /// submit, cleared if the popup is cancelled. None means the popup
    /// adds a new task.
    pub renaming: Option<String>,
    /// Task id whose tags are being edited: set when `t` opens the tag popup
    /// ([`crate::app::View::TaskTags`]), consumed on submit, cleared on cancel.
    /// Shares the `input` buffer with the add/rename popup (only one is ever
    /// open at a time).
    pub tagging: Option<String>,
    /// Task id awaiting a folder pick: set when `s` opens the picker, consumed
    /// by the pick handler, cleared if the picker is cancelled.
    pub pending_assign: Option<String>,
    /// Task id whose attach input is open
    /// ([`crate::app::View::TaskAttachInput`]): set when `A` (grid) or `a`
    /// (info popup) opens it, consumed on submit, cleared on cancel. Shares
    /// the `input` buffer with the other single-line task popups.
    pub attaching: Option<String>,
    /// Whether the attach input was opened from the Task Info popup, so
    /// submit/cancel return there instead of the grid.
    pub attach_from_info: bool,
    /// Attach-input mode: true (the default on open) treats the buffer as a
    /// typed note, false as a file path or URL. Tab flips it; the buffer
    /// survives the flip so a wrong-mode start costs nothing.
    pub attach_note: bool,
    /// Selected attachment index inside the Task Info popup
    /// ([`crate::app::View::TaskInfo`]).
    pub info_sel: usize,
    /// Frozen display order (task ids) for the In Progress column. The
    /// needs-input float is computed once on tab entry
    /// ([`crate::app::App::refresh_in_progress_order`]) instead of live in
    /// every render: live session state flips on scan ticks, and re-sorting
    /// the column by it swapped cards under the positional cursor — the
    /// exact bug the Sessions grid had before it moved to stable sort keys.
    /// Ids no longer in the column are skipped; tasks not in the list (e.g.
    /// assigned since entry) render after it in insertion order.
    pub in_progress_order: Vec<String>,
    /// Active board filter (`/`). Empty means no filter. Applied by
    /// [`Self::matches_filter`] to every column's cards and counts; edited
    /// live in [`crate::app::View::TaskFilter`] and kept when that view
    /// closes with Enter, so the board stays narrowed until Esc clears it.
    pub filter: String,
    /// Last `x`/`c` removal, one batch deep, for `u` to restore. Held in
    /// memory only — restarts forget it, but the on-disk archive
    /// (`tasks-archive.json`) still has every removed task.
    pub undo: Option<Vec<TaskState>>,
}

impl TasksView {
    pub(crate) fn new() -> Self {
        // NOTE: no migration here. `App::new()` runs in dozens of tests (and
        // hot-reload paths); the destructive tasks.json migration is invoked
        // exactly once, explicitly, from the binary entry point — see
        // `run()` in bin/src/main.rs.
        let (board, persistence_error) = match PersonalBoard::load_result() {
            Ok(board) => (board, None),
            Err(e) => (
                PersonalBoard::default(),
                Some(format!("task board load failed: {e}")),
            ),
        };
        Self {
            board,
            persistence_error,
            col: 0,
            row: 0,
            input: String::new(),
            context: String::new(),
            field: TaskField::default(),
            renaming: None,
            tagging: None,
            pending_assign: None,
            attaching: None,
            attach_from_info: false,
            attach_note: true,
            info_sel: 0,
            in_progress_order: Vec::new(),
            filter: String::new(),
            undo: None,
        }
    }

    /// Does `t` survive the active filter? An empty filter passes
    /// everything. The query fuzzy-matches the card text, or any tag as
    /// `#tag` — so a `#bug` query narrows to tagged cards without also
    /// matching arbitrary text.
    pub fn matches_filter(&self, t: &TaskState) -> bool {
        if self.filter.is_empty() {
            return true;
        }
        fuzzy_match(&self.filter, &t.prompt).is_some()
            || t.tags
                .iter()
                .any(|tag| fuzzy_match(&self.filter, &format!("#{}", tag)).is_some())
    }

    /// Reload the board from disk (picks up hand edits / other instances),
    /// keeping the cursor in range if the columns shrank.
    pub fn reload(&mut self) {
        match PersonalBoard::load_result() {
            Ok(board) => {
                self.board = board;
                self.persistence_error = None;
            }
            Err(e) => self.persistence_error = Some(format!("task board reload failed: {e}")),
        }
        self.clamp_row();
    }

    pub fn record_persistence_error(&mut self, operation: &str, error: impl std::fmt::Display) {
        self.persistence_error = Some(format!("task {operation} failed: {error}"));
    }

    pub fn take_persistence_error(&mut self) -> Option<String> {
        self.persistence_error.take()
    }

    pub fn col_status(&self) -> TaskStatus {
        let cols = visible_task_columns();
        cols[self.col.min(cols.len() - 1)]
    }

    /// Cards under visible column `col`, after the filter — the cursor's
    /// bound, so it must count exactly what the column renders.
    pub fn column_len(&self, col: usize) -> usize {
        let cols = visible_task_columns();
        let status = cols[col.min(cols.len() - 1)];
        column_statuses(status)
            .into_iter()
            .map(|s| {
                self.board
                    .column(s)
                    .into_iter()
                    .filter(|t| self.matches_filter(t))
                    .count()
            })
            .sum()
    }

    pub fn row_down(&mut self) {
        let len = self.column_len(self.col);
        if len > 0 && self.row + 1 < len {
            self.row += 1;
        }
    }

    pub fn row_up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
        }
    }

    pub fn col_right(&mut self) {
        if self.col + 1 < visible_task_columns().len() {
            self.col += 1;
            self.row = 0;
        }
    }

    pub fn col_left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            self.row = 0;
        }
    }

    /// Keep the row cursor inside the focused column after the board shrank
    /// (delete, clear-done, status move).
    pub fn clamp_row(&mut self) {
        let len = self.column_len(self.col);
        if self.row >= len {
            self.row = len.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planning_shown_keeps_all_four_columns() {
        assert_eq!(
            visible_columns(true),
            vec![
                TaskStatus::Backlog,
                TaskStatus::Planning,
                TaskStatus::Running,
                TaskStatus::Done,
            ]
        );
        // Each visible column maps to exactly its own status.
        for col in visible_columns(true) {
            assert_eq!(statuses_for(col, true), vec![col]);
        }
    }

    #[test]
    fn planning_hidden_drops_column_and_folds_into_in_progress() {
        assert_eq!(
            visible_columns(false),
            vec![TaskStatus::Backlog, TaskStatus::Running, TaskStatus::Done,]
        );
        // In Progress now also renders Planning cards; the other columns are
        // unchanged.
        assert_eq!(
            statuses_for(TaskStatus::Running, false),
            vec![TaskStatus::Planning, TaskStatus::Running]
        );
        assert_eq!(
            statuses_for(TaskStatus::Backlog, false),
            vec![TaskStatus::Backlog]
        );
        assert_eq!(
            statuses_for(TaskStatus::Done, false),
            vec![TaskStatus::Done]
        );
    }
}
