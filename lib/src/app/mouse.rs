//! The mouse on any tab with nothing over it: a left click on the tab strip
//! opens that tab, one on a sidebar, list or board puts the cursor on what
//! it lands on, and one on a sidebar-and-page tab's page gives the page the
//! keyboard. The wheel moves the cursor as the arrow keys would, or scrolls
//! the page under the pointer. A pane with the keyboard takes every other
//! event.

use super::{App, BuildsCommand, Command, Effect, GlobalCommand, HarnessCommand};
use super::{SessionsCommand, SessionsLayout, Tab, TasksCommand, View};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

impl App {
    /// Whether the hub wants the terminal's mouse: while a tab is on screen
    /// with nothing over it, or a pane is. Without capture the terminal
    /// turns the wheel into arrow keys, and over the dock those would move
    /// the list's cursor.
    pub fn wants_mouse(&self) -> bool {
        self.tab_on_screen() || self.view == View::TmuxPane
    }

    /// Whether the tab's body is on screen with nothing over it: the dock
    /// and the Agents and Builds pages are drawn inside it; every other
    /// view is a popup or an editor.
    fn tab_on_screen(&self) -> bool {
        matches!(
            self.view,
            View::Grid | View::Dock | View::AgentDetail | View::BuildLog
        )
    }

    fn split_on_screen(&self) -> bool {
        match self.current_tab {
            Tab::Sessions => self.sessions.layout == SessionsLayout::Split,
            Tab::Agents | Tab::Builds => true,
            Tab::Tasks | Tab::Metrics => false,
        }
    }

    /// Whether the keyboard is on the page rather than the list.
    fn page_has_keyboard(&self) -> bool {
        matches!(self.view, View::Dock | View::AgentDetail | View::BuildLog)
    }

    /// Route a captured mouse event. A click that opens a tab, moves the
    /// focus or the cursor goes no further; one inside the pane that already
    /// has the keyboard does. The dock's pane takes the wheel over it even
    /// without the keyboard.
    pub fn mouse(&mut self, ev: MouseEvent) -> Vec<Effect> {
        if let Some(tab) = self.clicked_tab(ev) {
            return self.open_tab(tab);
        }
        if self.click_moves_focus(ev) || self.click_moves_cursor(ev) {
            return Vec::new();
        }
        if let Some(pane) = self.focused_pane() {
            pane.send_mouse(ev);
            return Vec::new();
        }
        let up = match ev.kind {
            MouseEventKind::ScrollUp => true,
            MouseEventKind::ScrollDown => false,
            _ => return Vec::new(),
        };
        if self.view == View::Grid && self.current_tab == Tab::Sessions {
            if let Some(pane) = self.sessions.dock.pane_mut() {
                if pane.covers(ev.column, ev.row) {
                    pane.send_mouse(ev);
                    return Vec::new();
                }
            }
        }
        // Metrics moves its cursor outside the command layer.
        if self.view == View::Grid && self.current_tab == Tab::Metrics {
            if up {
                self.metrics_nav_up();
            } else {
                self.metrics_nav_down();
            }
            return Vec::new();
        }
        let on_page = self.split_on_screen()
            && self
                .render
                .split
                .page
                .contains(Position::new(ev.column, ev.row));
        match self.wheel_command(up, on_page) {
            Some(cmd) => self.execute(cmd),
            None => Vec::new(),
        }
    }

    /// The tab a left click on the strip lands on.
    fn clicked_tab(&self, ev: MouseEvent) -> Option<Tab> {
        let clicked = ev.kind == MouseEventKind::Down(MouseButton::Left);
        if !clicked || !self.tab_on_screen() {
            return None;
        }
        self.render.tabs.at(ev.column, ev.row)
    }

    /// Open `tab` with the keyboard on its list: a page that has the
    /// keyboard gives it back first, as `F1` would.
    fn open_tab(&mut self, tab: Tab) -> Vec<Effect> {
        if tab == self.current_tab {
            return Vec::new();
        }
        if self.page_has_keyboard() {
            self.focus_list(None);
        }
        self.execute(Command::Global(GlobalCommand::SetTab(tab)))
    }

    /// A wheel notch: the list's cursor moves, as its arrow keys would, or
    /// the page under the pointer scrolls.
    fn wheel_command(&self, up: bool, on_page: bool) -> Option<Command> {
        use BuildsCommand as B;
        use HarnessCommand as H;
        use SessionsCommand as S;
        use TasksCommand as T;
        Some(match (self.current_tab, &self.view, on_page) {
            (Tab::Sessions, View::Grid, _) => Command::Sessions(either(up, S::NavUp, S::NavDown)),
            (Tab::Tasks, View::Grid, _) => Command::Tasks(either(up, T::NavUp, T::NavDown)),
            (Tab::Agents, View::Grid | View::AgentDetail, false) => {
                Command::Harness(either(up, H::NavUp, H::NavDown))
            }
            (Tab::Agents, View::Grid | View::AgentDetail, true) => {
                Command::Harness(either(up, H::DetailUp, H::DetailDown))
            }
            (Tab::Builds, View::Grid | View::BuildLog, false) => {
                Command::Builds(either(up, B::NavUp, B::NavDown))
            }
            (Tab::Builds, View::Grid | View::BuildLog, true) => {
                Command::Builds(either(up, B::LogUp, B::LogDown))
            }
            _ => return None,
        })
    }

    /// A left click on the sidebar gives the list the keyboard, with the
    /// cursor on the item clicked; on the page, the page takes it.
    fn click_moves_focus(&mut self, ev: MouseEvent) -> bool {
        let clicked = ev.kind == MouseEventKind::Down(MouseButton::Left);
        if !clicked || !self.tab_on_screen() || !self.split_on_screen() {
            return false;
        }
        let at = Position::new(ev.column, ev.row);
        let split = &self.render.split;
        if split.sidebar.contains(at) {
            let item = split.item_at(ev.column, ev.row);
            self.focus_list(item);
            return true;
        }
        split.page.contains(at) && !self.page_has_keyboard() && self.focus_page()
    }

    /// A left click on a board card or on a session's row or card in the
    /// List and Cards layouts puts the cursor on it; one on a column's
    /// empty space moves the cursor to that column.
    fn click_moves_cursor(&mut self, ev: MouseEvent) -> bool {
        let clicked = ev.kind == MouseEventKind::Down(MouseButton::Left);
        if !clicked || self.view != View::Grid {
            return false;
        }
        let (x, y) = (ev.column, ev.row);
        let render = &self.render;
        match self.current_tab {
            Tab::Tasks => {
                if let Some((col, row)) = render.task_cards.at(x, y) {
                    self.tasks.focus(col, row);
                    return true;
                }
                let col = render.task_columns.at(x, y);
                if let Some(col) = col.filter(|col| *col != self.tasks.col) {
                    self.tasks.focus(col, 0);
                }
                col.is_some()
            }
            Tab::Sessions => {
                let session = render.sessions.at(x, y);
                if let Some((group, index)) = session {
                    self.sessions.sel_group = group;
                    self.sessions.sel_in_group = index;
                }
                session.is_some()
            }
            Tab::Agents | Tab::Builds | Tab::Metrics => false,
        }
    }

    fn focus_list(&mut self, item: Option<(usize, usize)>) {
        match self.current_tab {
            Tab::Sessions => {
                if let Some((group, index)) = item {
                    self.sessions.sel_group = group;
                    self.sessions.sel_in_group = index;
                }
                self.leave_dock();
            }
            Tab::Agents => {
                if let Some((_, index)) = item {
                    let was = self.harness.selected;
                    self.harness.select(index);
                    if self.harness.selected != was {
                        self.render.agent_detail_scroll = 0;
                    }
                }
                self.execute(Command::Harness(HarnessCommand::CloseDetail));
            }
            Tab::Builds => {
                if let Some((_, index)) = item {
                    self.builds.select(index);
                }
                self.execute(Command::Builds(BuildsCommand::CloseLog));
            }
            Tab::Tasks | Tab::Metrics => {}
        }
    }

    fn focus_page(&mut self) -> bool {
        match self.current_tab {
            Tab::Sessions => self.focus_dock(),
            Tab::Agents => {
                self.execute(Command::Harness(HarnessCommand::OpenDetail));
                true
            }
            Tab::Builds => {
                self.execute(Command::Builds(BuildsCommand::OpenLog));
                true
            }
            Tab::Tasks | Tab::Metrics => false,
        }
    }
}

/// The wheel's way: `up` or `down`.
fn either<T>(up: bool, up_cmd: T, down_cmd: T) -> T {
    if up {
        up_cmd
    } else {
        down_cmd
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::test_support::fake_session;
    use crate::harness::{AgentSnapshot, AgentState};
    use crate::models::SessionState;
    use crossterm::event::KeyModifiers;

    fn wheel(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// Two sessions in the sidebar, rows 1 and 2 under the project's header,
    /// and the dock to the right of column 32.
    fn split_app() -> App {
        let mut app = App::new();
        app.update_sessions(vec![
            fake_session("hal-a", SessionState::Idle),
            fake_session("hal-b", SessionState::Idle),
        ]);
        app.render.split = crate::app::SplitAreas {
            sidebar: ratatui::layout::Rect::new(0, 0, 32, 10),
            page: ratatui::layout::Rect::new(33, 0, 80, 10),
            rows: vec![None, Some((0, 0)), Some((0, 1))],
        };
        app
    }

    // The List layout takes the mouse too; the wheel over it moves the
    // cursor, as its arrow keys would.
    #[test]
    fn the_wheel_over_the_list_moves_the_cursor() {
        let mut app = App::new();
        app.update_sessions(vec![
            fake_session("hal-a", SessionState::Idle),
            fake_session("hal-b", SessionState::Idle),
        ]);
        app.sessions.layout = SessionsLayout::List;
        assert!(app.wants_mouse());

        app.mouse(wheel(MouseEventKind::ScrollDown));
        assert_eq!(app.sessions.sel_in_group, 1);
        app.mouse(wheel(MouseEventKind::ScrollUp));
        assert_eq!(app.sessions.sel_in_group, 0);
    }

    // Split takes the mouse even with no pane in the dock, so a click on
    // another session still lands.
    #[test]
    fn split_takes_the_mouse_without_a_pane() {
        assert!(split_app().wants_mouse());
    }

    // A click on a sidebar row moves the cursor there and the keyboard back
    // to the list; a click on a header only does the latter.
    #[test]
    fn a_click_on_the_sidebar_focuses_the_list_on_that_session() {
        let mut app = split_app();
        app.view = View::Dock;

        app.mouse(click(3, 2));
        assert_eq!(app.view, View::Grid);
        assert_eq!(app.sessions.sel_in_group, 1);

        app.view = View::Dock;
        app.mouse(click(3, 0));
        assert_eq!(app.view, View::Grid);
        assert_eq!(app.sessions.sel_in_group, 1);
    }

    #[test]
    fn a_click_on_the_dock_focuses_it() {
        let mut app = split_app();

        app.mouse(click(50, 5));
        assert_eq!(app.view, View::Dock);
    }

    // A popup over the list keeps the wheel off the cursor.
    #[test]
    fn the_wheel_leaves_the_cursor_alone_under_a_popup() {
        let mut app = App::new();
        app.update_sessions(vec![
            fake_session("hal-a", SessionState::Idle),
            fake_session("hal-b", SessionState::Idle),
        ]);
        app.view = View::RenameSession;

        app.mouse(wheel(MouseEventKind::ScrollDown));
        assert_eq!(app.sessions.sel_in_group, 0);
        assert!(!app.wants_mouse());
    }

    fn agent(name: &str) -> AgentSnapshot {
        let dir = std::path::Path::new("/tmp/agents").join(name);
        AgentSnapshot {
            name: name.into(),
            spec: crate::harness::spec::parse(&dir, "[prompt]\ninstruction = \"go\""),
            dir,
            state: AgentState::default(),
            notes: Vec::new(),
            events: Vec::new(),
            inbox_pending: 0,
        }
    }

    // On the Agents tab a click on a row selects that agent and gives the
    // list the keyboard; a click on the page gives the page the keyboard.
    #[test]
    fn a_click_moves_the_focus_on_the_agents_tab() {
        let mut app = App::new();
        app.current_tab = Tab::Agents;
        app.harness.update(vec![agent("a"), agent("b")]);
        app.render.split = crate::app::SplitAreas {
            sidebar: ratatui::layout::Rect::new(0, 0, 32, 10),
            page: ratatui::layout::Rect::new(33, 0, 80, 10),
            rows: vec![Some((0, 0)), Some((0, 1))],
        };
        assert!(app.wants_mouse());

        app.mouse(click(50, 5));
        assert_eq!(app.view, View::AgentDetail);

        app.mouse(click(3, 1));
        assert_eq!(app.view, View::Grid);
        assert_eq!(app.harness.selected, 1);
    }

    fn rect(x: u16, y: u16, width: u16, height: u16) -> ratatui::layout::Rect {
        ratatui::layout::Rect::new(x, y, width, height)
    }

    // A click on a tab opens it, with the keyboard on its list even when
    // the dock had it; landing on Metrics asks for its scan.
    #[test]
    fn a_click_on_a_tab_opens_it() {
        crate::test_util::with_temp_home(|| {
            let mut app = split_app();
            app.view = View::Dock;
            app.render.tabs.push(rect(2, 2, 10, 2), Tab::Tasks);
            app.render.tabs.push(rect(12, 2, 10, 2), Tab::Metrics);

            assert!(app.mouse(click(5, 3)).is_empty());
            assert_eq!(app.current_tab, Tab::Tasks);
            assert_eq!(app.view, View::Grid);

            let effects = app.mouse(click(15, 2));
            assert_eq!(app.current_tab, Tab::Metrics);
            assert_eq!(effects, vec![crate::app::Effect::SpawnMetricsScan]);
        });
    }

    // In the List and Cards layouts a click on a session's row or card
    // puts the cursor on it; the board's targets, left from the Tasks tab,
    // are not consulted.
    #[test]
    fn a_click_on_a_list_row_selects_that_session() {
        let mut app = App::new();
        app.update_sessions(vec![
            fake_session("hal-a", SessionState::Idle),
            fake_session("hal-b", SessionState::Idle),
        ]);
        app.sessions.layout = SessionsLayout::List;
        app.render.sessions.push(rect(0, 1, 80, 1), (0, 0));
        app.render.sessions.push(rect(0, 2, 80, 1), (0, 1));
        app.render.task_columns.push(rect(0, 0, 80, 10), 3);

        app.mouse(click(10, 2));
        assert_eq!(app.sessions.sel_in_group, 1);
        assert_eq!(app.tasks.col, 0);
    }

    // On the board a click on a card focuses its column and the card; one
    // on another column's empty space moves the cursor to that column.
    #[test]
    fn a_click_on_the_board_moves_its_cursor() {
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            app.current_tab = Tab::Tasks;
            app.tasks.board.add("first").unwrap();
            app.tasks.board.add("second").unwrap();
            app.render.task_columns.push(rect(0, 0, 30, 20), 0);
            app.render.task_columns.push(rect(30, 0, 30, 20), 1);
            app.render.task_cards.push(rect(1, 1, 28, 5), (0, 0));
            app.render.task_cards.push(rect(1, 6, 28, 5), (0, 1));
            assert!(app.wants_mouse());

            app.mouse(click(10, 8));
            assert_eq!((app.tasks.col, app.tasks.row), (0, 1));

            app.mouse(click(40, 15));
            assert_eq!((app.tasks.col, app.tasks.row), (1, 0));
        });
    }

    #[test]
    fn the_wheel_on_the_board_moves_the_card_cursor() {
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            app.current_tab = Tab::Tasks;
            app.tasks.board.add("first").unwrap();
            app.tasks.board.add("second").unwrap();

            app.mouse(wheel(MouseEventKind::ScrollDown));
            assert_eq!(app.tasks.row, 1);
            app.mouse(wheel(MouseEventKind::ScrollUp));
            assert_eq!(app.tasks.row, 0);
        });
    }
}
