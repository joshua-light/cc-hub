//! The mouse on a sidebar-and-page tab: a left click puts the keyboard on
//! the side it lands on, and the wheel scrolls the side under the pointer.
//! A pane with the keyboard takes every event.

use super::{App, BuildsCommand, Command, HarnessCommand, SessionsCommand, SessionsLayout};
use super::{Tab, View};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

impl App {
    /// Whether the hub wants the terminal's mouse: while a pane or a
    /// sidebar-and-page tab is on screen. Without capture the terminal turns
    /// the wheel into arrow keys, and over the dock those would move the
    /// list's cursor.
    pub fn wants_mouse(&self) -> bool {
        match self.view {
            View::TmuxPane | View::Dock | View::AgentDetail | View::BuildLog => true,
            View::Grid => self.split_on_screen() || self.sessions.dock.pane().is_some(),
            _ => false,
        }
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

    /// Route a captured mouse event. The click that moves the focus goes no
    /// further; one inside the pane that already has it does. The dock's
    /// pane takes the wheel over it even without the keyboard.
    pub fn mouse(&mut self, ev: MouseEvent) {
        if self.click_moves_focus(ev) {
            return;
        }
        if let Some(pane) = self.focused_pane() {
            pane.send_mouse(ev);
            return;
        }
        let up = match ev.kind {
            MouseEventKind::ScrollUp => true,
            MouseEventKind::ScrollDown => false,
            _ => return,
        };
        if self.view == View::Grid {
            if let Some(pane) = self.sessions.dock.pane_mut() {
                if pane.covers(ev.column, ev.row) {
                    pane.send_mouse(ev);
                    return;
                }
            }
        }
        let on_page = self.split_on_screen()
            && self
                .render
                .split
                .page
                .contains(Position::new(ev.column, ev.row));
        if let Some(cmd) = self.wheel_command(up, on_page) {
            self.execute(cmd);
        }
    }

    /// A wheel notch: the list's cursor moves, as its arrow keys would, or
    /// the page under the pointer scrolls.
    fn wheel_command(&self, up: bool, on_page: bool) -> Option<Command> {
        use BuildsCommand as B;
        use HarnessCommand as H;
        use SessionsCommand as S;
        Some(match (self.current_tab, &self.view, on_page) {
            (Tab::Sessions, View::Grid, _) => Command::Sessions(either(up, S::NavUp, S::NavDown)),
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
        let between_sides = matches!(
            self.view,
            View::Grid | View::Dock | View::AgentDetail | View::BuildLog
        );
        if !clicked || !between_sides || !self.split_on_screen() {
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
            fake_session("cchub-a", SessionState::Idle),
            fake_session("cchub-b", SessionState::Idle),
        ]);
        app.render.split = crate::app::SplitAreas {
            sidebar: ratatui::layout::Rect::new(0, 0, 32, 10),
            page: ratatui::layout::Rect::new(33, 0, 80, 10),
            rows: vec![None, Some((0, 0)), Some((0, 1))],
        };
        app
    }

    // Away from Split, with no pane on screen, the terminal keeps the mouse;
    // a wheel that does arrive over the list moves the cursor, as its arrow
    // keys would.
    #[test]
    fn the_wheel_over_the_list_moves_the_cursor() {
        let mut app = App::new();
        app.update_sessions(vec![
            fake_session("cchub-a", SessionState::Idle),
            fake_session("cchub-b", SessionState::Idle),
        ]);
        app.sessions.layout = SessionsLayout::List;
        assert!(!app.wants_mouse());

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
            fake_session("cchub-a", SessionState::Idle),
            fake_session("cchub-b", SessionState::Idle),
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
}
