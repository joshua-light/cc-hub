use crate::models::{SessionDetail, SessionInfo};
use std::time::Instant;

#[derive(Clone, Debug, PartialEq)]
pub enum View {
    Grid,
    Popup,
}

pub struct App {
    pub sessions: Vec<SessionInfo>,
    pub selected: usize,
    pub view: View,
    pub detail: Option<SessionDetail>,
    pub detail_loading: bool,
    pub popup_scroll: u16,
    pub should_quit: bool,
    pub last_refresh: Instant,
    pub grid_cols: u16,
}

impl App {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            selected: 0,
            view: View::Grid,
            detail: None,
            detail_loading: false,
            popup_scroll: 0,
            should_quit: false,
            last_refresh: Instant::now(),
            grid_cols: 3,
        }
    }

    pub fn move_right(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.sessions.len();
    }

    pub fn move_left(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        if self.selected == 0 {
            self.selected = self.sessions.len() - 1;
        } else {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let cols = self.grid_cols as usize;
        let next = self.selected + cols;
        if next < self.sessions.len() {
            self.selected = next;
        }
    }

    pub fn move_up(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let cols = self.grid_cols as usize;
        if self.selected >= cols {
            self.selected -= cols;
        }
    }

    pub fn scroll_down(&mut self) {
        self.popup_scroll = self.popup_scroll.saturating_add(3);
    }

    pub fn scroll_up(&mut self) {
        self.popup_scroll = self.popup_scroll.saturating_sub(3);
    }

    pub fn enter_popup(&mut self) {
        self.view = View::Popup;
        self.detail_loading = true;
        self.popup_scroll = 0;
    }

    pub fn close_popup(&mut self) {
        self.view = View::Grid;
        self.detail = None;
        self.detail_loading = false;
        self.popup_scroll = 0;
    }

    pub fn selected_session_id(&self) -> Option<String> {
        self.sessions.get(self.selected).map(|s| s.session_id.clone())
    }

    pub fn update_sessions(&mut self, sessions: Vec<SessionInfo>) {
        let prev_id = self.selected_session_id();
        self.sessions = sessions;
        self.last_refresh = Instant::now();

        if let Some(id) = prev_id {
            if let Some(pos) = self.sessions.iter().position(|s| s.session_id == id) {
                self.selected = pos;
                return;
            }
        }
        if !self.sessions.is_empty() {
            self.selected = self.selected.min(self.sessions.len() - 1);
        } else {
            self.selected = 0;
        }
    }

    pub fn update_detail(&mut self, detail: SessionDetail) {
        self.detail = Some(detail);
        self.detail_loading = false;
    }

    pub fn update_grid_cols(&mut self, width: u16) {
        let cell_width = 42u16;
        self.grid_cols = (width / cell_width).max(1);
    }

    pub fn alive_count(&self) -> usize {
        self.sessions.iter().filter(|s| s.alive).count()
    }
}
