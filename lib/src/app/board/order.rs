//! Board column order, cursor resolution, and the board filter.

use super::{column_statuses, visible_task_columns};
use crate::app::{App, View};
use crate::models::SessionState;
use crate::tasks::store::{TaskState, TaskStatus};

impl App {
    /// Tasks in `status` in display order: To-Do and Done keep the board's
    /// insertion order; the live columns (Planning and In Progress) follow
    /// the frozen needs-input float captured on tab entry
    /// ([`super::TasksView::in_progress_order`]), with tasks that joined
    /// since after it in insertion order. Every column then sorts by
    /// priority (P1 at the top). Selection ([`Self::selected_board_task`])
    /// and focus ([`Self::focus_task`]) resolve against this same ordering,
    /// so cursor row N is always the Nth rendered card.
    pub fn task_column(&self, status: TaskStatus) -> Vec<&TaskState> {
        self.cards_in(&[status])
    }

    /// Recompute the live columns' display order: cards whose agent waits on
    /// a human float to the top of their column (stable within each group) —
    /// blocked-on-input first, then idle agents (a plan or an implementation
    /// sitting ready for a verdict), then everything else. Spans both
    /// Planning and In Progress. Called on Tasks-tab entry — not on scan
    /// ticks — so live state flips never reorder cards under the cursor
    /// while the user is navigating; the float settles each time the tab is
    /// (re-)opened.
    pub fn refresh_in_progress_order(&mut self) {
        let order: Vec<String> = {
            let by_tmux = self.sessions_by_tmux();
            let mut order = Vec::new();
            for status in [TaskStatus::Planning, TaskStatus::Running] {
                let mut tasks = self.tasks.board.column(status);
                tasks.sort_by_key(|t| match t.tmux.as_deref().and_then(|n| by_tmux.get(n)) {
                    Some(s) if s.needs_attention() => 0u8,
                    Some(s) if s.state == SessionState::Idle => 1,
                    _ => 2,
                });
                order.extend(tasks.iter().map(|t| t.task_id.clone()));
            }
            order
        };
        self.tasks.in_progress_order = order;
    }

    /// Cards rendered under one visible board column, in display order. Same
    /// as [`Self::task_column`] for a normal column; when the Planning column
    /// is hidden, the In Progress column also carries Planning cards (folded
    /// in via [`column_statuses`]) so plan-ready work stays visible. The
    /// merged set keeps the live columns' needs-input float and priority sort.
    pub fn task_display_column(&self, col: TaskStatus) -> Vec<&TaskState> {
        self.cards_in(&column_statuses(col))
    }

    /// The filtered cards of `statuses` in the order [`Self::task_column`]
    /// documents.
    fn cards_in(&self, statuses: &[TaskStatus]) -> Vec<&TaskState> {
        let mut tasks: Vec<&TaskState> = statuses
            .iter()
            .flat_map(|s| self.tasks.board.column(*s))
            .filter(|t| self.tasks.matches_filter(t))
            .collect();
        if statuses
            .iter()
            .any(|s| matches!(s, TaskStatus::Planning | TaskStatus::Running))
        {
            let frozen = |id: &str| self.tasks.in_progress_order.iter().position(|x| x == id);
            // Stable sort: ids missing from the frozen order all key to MAX
            // and keep their relative insertion order at the tail.
            tasks.sort_by_key(|t| frozen(&t.task_id).unwrap_or(usize::MAX));
        }
        // Priority is the primary order in every column. The sort is stable,
        // so equal-priority tasks keep the order established above.
        tasks.sort_by_key(|t| t.priority);
        tasks
    }

    /// The task under the kanban cursor, resolved against the display
    /// ordering of [`Self::task_display_column`].
    pub fn selected_board_task(&self) -> Option<&TaskState> {
        self.task_display_column(self.tasks.col_status())
            .get(self.tasks.row)
            .copied()
    }

    /// Move the cursor to `id` wherever it now renders (e.g. after a status
    /// transition or an assignment carried the card to another column).
    /// Resolves against the visible columns, so a Planning card lands under
    /// In Progress when the Planning column is hidden.
    pub fn focus_task(&mut self, id: &str) {
        for (ci, col) in visible_task_columns().iter().enumerate() {
            let row = self
                .task_display_column(*col)
                .iter()
                .position(|t| t.task_id == id);
            if let Some(ri) = row {
                self.tasks.col = ci;
                self.tasks.row = ri;
                return;
            }
        }
    }

    /// `/` on the board: start editing the filter. Whatever was typed
    /// before stays as the starting query, so `/` re-opens an applied
    /// filter for refinement.
    pub fn enter_task_filter(&mut self) {
        self.view = View::TaskFilter;
    }

    /// Enter while filtering: keep the query applied and go back to normal
    /// board navigation. An empty query just closes the bar.
    pub fn apply_task_filter(&mut self) {
        self.view = View::Grid;
        self.tasks.clamp_row();
    }

    /// Esc (while filtering, or on a filtered board): drop the filter and
    /// show the full board again.
    pub fn clear_task_filter(&mut self) {
        self.tasks.filter.clear();
        self.view = View::Grid;
        self.tasks.clamp_row();
    }

    pub fn task_filter_push(&mut self, c: char) {
        self.tasks.filter.push(c);
        self.tasks.clamp_row();
    }

    pub fn task_filter_pop(&mut self) {
        self.tasks.filter.pop();
        self.tasks.clamp_row();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::test_support::fake_session;
    use crate::app::Tab;
    use crate::tasks::store::TaskPriority;
    use crate::test_util::with_temp_home;

    // Assignment lands cards in Planning, so the float tests inspect that
    // column; Planning sorts exactly like In Progress.
    fn column_order(app: &App) -> Vec<String> {
        app.task_column(TaskStatus::Planning)
            .iter()
            .map(|t| t.task_id.clone())
            .collect()
    }

    #[test]
    fn live_columns_float_needs_input_and_cursor_follows() {
        with_temp_home(|| {
            let mut app = App::new();
            let a = app.tasks.board.add("a").unwrap().unwrap();
            let b = app.tasks.board.add("b").unwrap().unwrap();
            let c = app.tasks.board.add("c").unwrap().unwrap();
            app.tasks
                .board
                .assign(&a, "/tmp", "claude", "mux-a")
                .unwrap();
            app.tasks
                .board
                .assign(&b, "/tmp", "claude", "mux-b")
                .unwrap();
            app.tasks
                .board
                .assign(&c, "/tmp", "claude", "mux-c")
                .unwrap();
            app.sessions.last_sessions = vec![
                fake_session("mux-a", SessionState::Processing),
                fake_session("mux-b", SessionState::WaitingForInput),
                fake_session("mux-c", SessionState::Question),
            ];
            app.refresh_in_progress_order();

            // Both flavors of blocked-on-input rise above the working
            // agent, keeping their relative insertion order. Assignment
            // lands the cards in Planning, which sorts like In Progress.
            let order: Vec<&str> = app
                .task_column(TaskStatus::Planning)
                .iter()
                .map(|t| t.task_id.as_str())
                .collect();
            assert_eq!(order, vec![b.as_str(), c.as_str(), a.as_str()]);

            // The same ordering applies once the cards are promoted.
            for id in [&a, &b, &c] {
                app.tasks.board.set_status(id, TaskStatus::Running).unwrap();
            }
            let order: Vec<&str> = app
                .task_column(TaskStatus::Running)
                .iter()
                .map(|t| t.task_id.as_str())
                .collect();
            assert_eq!(order, vec![b.as_str(), c.as_str(), a.as_str()]);

            // Selection and focus resolve against the rendered order,
            // not the board file's insertion order. Derive In Progress's
            // column index so the test holds whether or not the optional
            // Planning column is configured in.
            let ip_col = visible_task_columns()
                .iter()
                .position(|s| *s == TaskStatus::Running)
                .unwrap();
            app.focus_task(&a);
            assert_eq!((app.tasks.col, app.tasks.row), (ip_col, 2));
            assert_eq!(app.selected_board_task().unwrap().task_id, a);
        });
    }

    #[test]
    fn scan_state_flips_do_not_reorder_under_the_cursor() {
        with_temp_home(|| {
            let mut app = App::new();
            let a = app.tasks.board.add("a").unwrap().unwrap();
            let b = app.tasks.board.add("b").unwrap().unwrap();
            app.tasks
                .board
                .assign(&a, "/tmp", "claude", "mux-a")
                .unwrap();
            app.tasks
                .board
                .assign(&b, "/tmp", "claude", "mux-b")
                .unwrap();
            app.sessions.last_sessions = vec![
                fake_session("mux-a", SessionState::Processing),
                fake_session("mux-b", SessionState::WaitingForInput),
            ];
            app.refresh_in_progress_order();
            assert_eq!(column_order(&app), vec![b.clone(), a.clone()]);
            app.focus_task(&a);

            // A scan tick flips both states. The frozen order (and so
            // the card under the cursor) must not move until the tab is
            // re-entered.
            app.sessions.last_sessions = vec![
                fake_session("mux-a", SessionState::Question),
                fake_session("mux-b", SessionState::Idle),
            ];
            assert_eq!(column_order(&app), vec![b.clone(), a.clone()]);
            assert_eq!(app.selected_board_task().unwrap().task_id, a);

            // A task assigned mid-tab joins below the frozen order
            // instead of re-shuffling it.
            let c = app.tasks.board.add("c").unwrap().unwrap();
            app.tasks
                .board
                .assign(&c, "/tmp", "claude", "mux-c")
                .unwrap();
            assert_eq!(column_order(&app), vec![b, a, c]);
        });
    }

    #[test]
    fn tab_reentry_refloats_and_keeps_cursor_on_the_same_task() {
        with_temp_home(|| {
            let mut app = App::new();
            let a = app.tasks.board.add("a").unwrap().unwrap();
            let b = app.tasks.board.add("b").unwrap().unwrap();
            app.tasks
                .board
                .assign(&a, "/tmp", "claude", "mux-a")
                .unwrap();
            app.tasks
                .board
                .assign(&b, "/tmp", "claude", "mux-b")
                .unwrap();
            app.set_tab(Tab::Tasks);
            assert_eq!(column_order(&app), vec![a.clone(), b.clone()]);
            app.focus_task(&b);

            // While the user is elsewhere, b's agent blocks on input;
            // coming back re-floats the column and follows b to its new
            // row instead of leaving the cursor parked on a's card.
            app.set_tab(Tab::Sessions);
            app.sessions.last_sessions = vec![fake_session("mux-b", SessionState::WaitingForInput)];
            app.set_tab(Tab::Tasks);
            assert_eq!(column_order(&app), vec![b.clone(), a]);
            assert_eq!((app.tasks.col, app.tasks.row), (1, 0));
            assert_eq!(app.selected_board_task().unwrap().task_id, b);
        });
    }

    #[test]
    fn columns_sort_by_priority_stable_within_level() {
        with_temp_home(|| {
            let mut app = App::new();
            // All start at the default P3 and keep insertion order.
            let a = app.tasks.board.add("a").unwrap().unwrap();
            let b = app.tasks.board.add("b").unwrap().unwrap();
            let c = app.tasks.board.add("c").unwrap().unwrap();
            let d = app.tasks.board.add("d").unwrap().unwrap();
            app.tasks.board.set_priority(&c, TaskPriority::P1).unwrap();
            app.tasks.board.set_priority(&d, TaskPriority::P2).unwrap();

            // P1, then P2, then the untouched P3s in their original order.
            let order: Vec<String> = app
                .task_column(TaskStatus::Backlog)
                .iter()
                .map(|t| t.task_id.clone())
                .collect();
            assert_eq!(order, vec![c, d, a, b]);
        });
    }

    #[test]
    fn priority_outranks_in_progress_needs_input_float() {
        with_temp_home(|| {
            let mut app = App::new();
            let a = app.tasks.board.add("a").unwrap().unwrap();
            let b = app.tasks.board.add("b").unwrap().unwrap();
            app.tasks
                .board
                .assign(&a, "/tmp", "claude", "mux-a")
                .unwrap();
            app.tasks
                .board
                .assign(&b, "/tmp", "claude", "mux-b")
                .unwrap();
            // a is just working; b is blocked on input, so the float alone
            // would put b first.
            app.sessions.last_sessions = vec![
                fake_session("mux-a", SessionState::Processing),
                fake_session("mux-b", SessionState::WaitingForInput),
            ];
            app.refresh_in_progress_order();
            assert_eq!(column_order(&app), vec![b.clone(), a.clone()]);

            // Raising a to P1 lifts it above b despite b needing input —
            // priority is the primary key, the float only a tie-break.
            app.tasks.board.set_priority(&a, TaskPriority::P1).unwrap();
            assert_eq!(column_order(&app), vec![a, b]);
        });
    }

    #[test]
    fn idle_agents_float_above_working_below_needs_input() {
        with_temp_home(|| {
            let mut app = App::new();
            let a = app.tasks.board.add("a").unwrap().unwrap();
            let b = app.tasks.board.add("b").unwrap().unwrap();
            let c = app.tasks.board.add("c").unwrap().unwrap();
            app.tasks
                .board
                .assign(&a, "/tmp", "claude", "mux-a")
                .unwrap();
            app.tasks
                .board
                .assign(&b, "/tmp", "claude", "mux-b")
                .unwrap();
            app.tasks
                .board
                .assign(&c, "/tmp", "claude", "mux-c")
                .unwrap();
            app.sessions.last_sessions = vec![
                fake_session("mux-a", SessionState::Processing),
                fake_session("mux-b", SessionState::Idle),
                fake_session("mux-c", SessionState::WaitingForInput),
            ];
            app.refresh_in_progress_order();
            // Blocked-on-input first, then the idle agent (a plan or an
            // implementation waiting for a verdict), then the one still
            // working.
            assert_eq!(column_order(&app), vec![c, b, a]);
        });
    }

    fn type_filter(app: &mut App, query: &str) {
        app.enter_task_filter();
        for c in query.chars() {
            app.task_filter_push(c);
        }
    }

    #[test]
    fn filter_narrows_columns_counts_and_selection_consistently() {
        with_temp_home(|| {
            let mut app = App::new();
            let a = app.tasks.board.add("fix the parser").unwrap().unwrap();
            let b = app.tasks.board.add("write docs").unwrap().unwrap();
            app.tasks.board.set_tags(&b, vec!["docs".into()]).unwrap();
            app.tasks.board.add("refactor scanner").unwrap().unwrap();

            type_filter(&mut app, "parser");
            assert_eq!(app.view, View::TaskFilter);
            let col: Vec<&str> = app
                .task_column(TaskStatus::Backlog)
                .iter()
                .map(|t| t.task_id.as_str())
                .collect();
            assert_eq!(col, vec![a.as_str()]);
            // The cursor bound counts exactly what renders.
            assert_eq!(app.tasks.column_len(0), 1);
            assert_eq!(app.selected_board_task().unwrap().task_id, a);

            // Tags match as `#tag`, so a `#` query reaches only tagged
            // cards — "docs" also appears in b's text, but "#docs" only
            // in its tag.
            app.clear_task_filter();
            type_filter(&mut app, "#docs");
            let col: Vec<&str> = app
                .task_column(TaskStatus::Backlog)
                .iter()
                .map(|t| t.task_id.as_str())
                .collect();
            assert_eq!(col, vec![b.as_str()]);

            // Enter keeps the filter applied; Esc clears it.
            app.apply_task_filter();
            assert_eq!(app.view, View::Grid);
            assert_eq!(app.tasks.column_len(0), 1);
            app.clear_task_filter();
            assert_eq!(app.tasks.column_len(0), 3);
        });
    }

    #[test]
    fn narrowing_filter_clamps_the_cursor() {
        with_temp_home(|| {
            let mut app = App::new();
            app.tasks.board.add("alpha").unwrap().unwrap();
            app.tasks.board.add("beta").unwrap().unwrap();
            let c = app.tasks.board.add("beta two").unwrap().unwrap();
            app.focus_task(&c);
            assert_eq!(app.tasks.row, 2);
            type_filter(&mut app, "beta");
            // Two cards survive; the row-2 cursor is pulled in range so
            // the selection stays on a real card.
            assert_eq!(app.tasks.column_len(0), 2);
            assert!(app.tasks.row < 2);
            assert!(app.selected_board_task().is_some());
        });
    }
}
