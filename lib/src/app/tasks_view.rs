//! Per-tab state for the Tasks tab: the persistent board plus the kanban
//! cursor (column, row), the add-task input buffer, and the id of a task
//! waiting on a folder pick for agent assignment.

use crate::tasks::{TaskBoard, TaskItemStatus};

/// Column order on the board. Indices are the `col` cursor values.
pub const TASK_COLUMNS: [TaskItemStatus; 4] = [
    TaskItemStatus::Todo,
    TaskItemStatus::Planning,
    TaskItemStatus::InProgress,
    TaskItemStatus::Done,
];

pub struct TasksView {
    pub board: TaskBoard,
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
}

impl TasksView {
    pub(crate) fn new() -> Self {
        Self {
            board: TaskBoard::load(),
            col: 0,
            row: 0,
            input: String::new(),
            renaming: None,
            tagging: None,
            pending_assign: None,
            in_progress_order: Vec::new(),
        }
    }

    /// Reload the board from disk (picks up hand edits / other instances),
    /// keeping the cursor in range if the columns shrank.
    pub fn reload(&mut self) {
        self.board = TaskBoard::load();
        self.clamp_row();
    }

    pub fn col_status(&self) -> TaskItemStatus {
        TASK_COLUMNS[self.col.min(TASK_COLUMNS.len() - 1)]
    }

    pub fn column_len(&self, col: usize) -> usize {
        self.board
            .column(TASK_COLUMNS[col.min(TASK_COLUMNS.len() - 1)])
            .len()
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
        if self.col + 1 < TASK_COLUMNS.len() {
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
