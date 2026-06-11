//! Per-tab state for the Tasks tab: the persistent board plus the kanban
//! cursor (column, row), the add-task input buffer, and the id of a task
//! waiting on a folder pick for agent assignment.

use crate::tasks::{TaskBoard, TaskItemStatus};

/// Column order on the board. Indices are the `col` cursor values.
pub const TASK_COLUMNS: [TaskItemStatus; 3] = [
    TaskItemStatus::Todo,
    TaskItemStatus::InProgress,
    TaskItemStatus::Done,
];

pub struct TasksView {
    pub board: TaskBoard,
    /// Focused column (index into [`TASK_COLUMNS`]).
    pub col: usize,
    /// Row cursor within the focused column.
    pub row: usize,
    /// In-progress text for the add-task popup ([`crate::app::View::TaskInput`]).
    pub input: String,
    /// Task id awaiting a folder pick: set when `s` opens the picker, consumed
    /// by the pick handler, cleared if the picker is cancelled.
    pub pending_assign: Option<String>,
}

impl TasksView {
    pub(crate) fn new() -> Self {
        Self {
            board: TaskBoard::load(),
            col: 0,
            row: 0,
            input: String::new(),
            pending_assign: None,
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
        self.board.column(TASK_COLUMNS[col.min(TASK_COLUMNS.len() - 1)]).len()
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
