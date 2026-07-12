//! Per-tab state for the Tasks tab: the persistent board plus the kanban
//! cursor (column, row), the add-task input buffer, and the id of a task
//! waiting on a folder pick for agent assignment.

use crate::config;
use crate::fuzzy::fuzzy_match;
use crate::tasks::{TaskBoard, TaskItem, TaskItemStatus};

/// Full board column order. Planning is optional at render time (see
/// [`visible_task_columns`]); this stays the canonical set the board logic
/// and on-disk statuses are defined against.
pub const TASK_COLUMNS: [TaskItemStatus; 4] = [
    TaskItemStatus::Todo,
    TaskItemStatus::Planning,
    TaskItemStatus::InProgress,
    TaskItemStatus::Done,
];

/// Columns shown on the Tasks board, in cursor (`col`) order. The Planning
/// column is optional (`ui.show_planning_column`); when off it's dropped here
/// and its cards fold into In Progress (see [`column_statuses`]), so the
/// status stays reachable — only the dedicated column disappears.
pub fn visible_task_columns() -> Vec<TaskItemStatus> {
    visible_columns(config::get().ui.show_planning_column)
}

/// The board statuses one visible column renders. One status per column,
/// except In Progress absorbs Planning when the Planning column is hidden so
/// plan-ready cards still appear (and Space still approves them — the action
/// keys off the card's own status).
pub fn column_statuses(col: TaskItemStatus) -> Vec<TaskItemStatus> {
    statuses_for(col, config::get().ui.show_planning_column)
}

/// Pure core of [`visible_task_columns`], split out so the column logic is
/// testable without the global config singleton.
fn visible_columns(show_planning: bool) -> Vec<TaskItemStatus> {
    TASK_COLUMNS
        .into_iter()
        .filter(|s| show_planning || *s != TaskItemStatus::Planning)
        .collect()
}

/// Pure core of [`column_statuses`].
fn statuses_for(col: TaskItemStatus, show_planning: bool) -> Vec<TaskItemStatus> {
    if col == TaskItemStatus::InProgress && !show_planning {
        vec![TaskItemStatus::Planning, TaskItemStatus::InProgress]
    } else {
        vec![col]
    }
}

pub struct TasksView {
    pub board: TaskBoard,
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
    pub undo: Option<Vec<TaskItem>>,
}

impl TasksView {
    pub(crate) fn new() -> Self {
        let (board, persistence_error) = match TaskBoard::load_result() {
            Ok(board) => (board, None),
            Err(e) => (
                TaskBoard::default(),
                Some(format!("task board load failed: {e}")),
            ),
        };
        Self {
            board,
            persistence_error,
            col: 0,
            row: 0,
            input: String::new(),
            renaming: None,
            tagging: None,
            pending_assign: None,
            in_progress_order: Vec::new(),
            filter: String::new(),
            undo: None,
        }
    }

    /// Does `t` survive the active filter? An empty filter passes
    /// everything. The query fuzzy-matches the card text, or any tag as
    /// `#tag` — so a `#bug` query narrows to tagged cards without also
    /// matching arbitrary text.
    pub fn matches_filter(&self, t: &TaskItem) -> bool {
        if self.filter.is_empty() {
            return true;
        }
        fuzzy_match(&self.filter, &t.text).is_some()
            || t.tags
                .iter()
                .any(|tag| fuzzy_match(&self.filter, &format!("#{}", tag)).is_some())
    }

    /// Reload the board from disk (picks up hand edits / other instances),
    /// keeping the cursor in range if the columns shrank.
    pub fn reload(&mut self) {
        match TaskBoard::load_result() {
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

    pub fn col_status(&self) -> TaskItemStatus {
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
                TaskItemStatus::Todo,
                TaskItemStatus::Planning,
                TaskItemStatus::InProgress,
                TaskItemStatus::Done,
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
            vec![
                TaskItemStatus::Todo,
                TaskItemStatus::InProgress,
                TaskItemStatus::Done,
            ]
        );
        // In Progress now also renders Planning cards; the other columns are
        // unchanged.
        assert_eq!(
            statuses_for(TaskItemStatus::InProgress, false),
            vec![TaskItemStatus::Planning, TaskItemStatus::InProgress]
        );
        assert_eq!(
            statuses_for(TaskItemStatus::Todo, false),
            vec![TaskItemStatus::Todo]
        );
        assert_eq!(
            statuses_for(TaskItemStatus::Done, false),
            vec![TaskItemStatus::Done]
        );
    }
}
