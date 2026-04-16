use crate::acks::Acks;
use crate::conversation::StateExplanation;
use crate::live_view::LiveView;
use crate::models::{ProjectGroup, SessionDetail, SessionInfo, SessionState};
use crate::usage::UsageInfo;
use ratatui::text::Line;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

pub const STATUS_MSG_TTL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq)]
pub enum View {
    Grid,
    Popup,
    LiveTail,
    ConfirmClose,
    StateDebug,
}

#[derive(Clone, Debug)]
pub struct PendingClose {
    pub pid: u32,
    pub display: String,
}

pub struct App {
    pub groups: Vec<ProjectGroup>,
    pub sel_group: usize,
    pub sel_in_group: usize,
    pub view: View,
    pub detail: Option<SessionDetail>,
    pub detail_loading: bool,
    pub popup_scroll: u16,
    pub grid_scroll: u16,
    pub should_quit: bool,
    pub last_refresh: Instant,
    pub grid_cols: u16,
    pub live_view: Option<LiveView>,
    pub acks: Acks,
    pub status_msg: Option<(String, Instant)>,
    pub pending_close: Option<PendingClose>,
    pub state_debug: Option<(SessionInfo, StateExplanation)>,
    pub state_debug_lines: Vec<Line<'static>>,
    pub state_debug_scroll: u16,
    pub usage: Option<UsageInfo>,
    pub usage_line: Line<'static>,
}

impl App {
    pub fn new() -> Self {
        Self {
            groups: Vec::new(),
            sel_group: 0,
            sel_in_group: 0,
            view: View::Grid,
            detail: None,
            detail_loading: false,
            popup_scroll: 0,
            grid_scroll: 0,
            should_quit: false,
            last_refresh: Instant::now(),
            grid_cols: 3,
            live_view: None,
            acks: Acks::new(),
            status_msg: None,
            pending_close: None,
            state_debug: None,
            state_debug_lines: Vec::new(),
            state_debug_scroll: 0,
            usage: None,
            usage_line: Line::default(),
        }
    }

    pub fn update_usage(&mut self, usage: UsageInfo, rendered: Line<'static>) {
        self.usage = Some(usage);
        self.usage_line = rendered;
    }

    pub fn enter_confirm_close(&mut self) {
        let Some(session) = self.selected_session_info() else {
            return;
        };
        self.pending_close = Some(PendingClose {
            pid: session.pid,
            display: format!("{} (PID {})", session.project_name, session.pid),
        });
        self.view = View::ConfirmClose;
    }

    pub fn cancel_confirm_close(&mut self) {
        self.pending_close = None;
        self.view = View::Grid;
    }

    pub fn take_pending_close(&mut self) -> Option<PendingClose> {
        self.view = View::Grid;
        self.pending_close.take()
    }

    pub fn set_status(&mut self, msg: String) {
        self.status_msg = Some((msg, Instant::now()));
    }

    /// Stamp an ack for the currently-selected session, forcing it to display
    /// as Idle until new activity advances its watermark. Works for any
    /// non-Idle state (WaitingForInput or Processing).
    /// Returns true if an ack was recorded.
    pub fn ack_selected(&mut self) -> bool {
        let Some(session) = self.selected_session_info() else {
            return false;
        };
        if session.state == SessionState::Idle {
            return false;
        }
        let id = session.session_id.clone();
        let watermark = session.last_activity;
        self.acks.ack(&id, watermark);
        // Apply immediately so the UI reflects the ack before the next scan tick.
        if let Some(s) = self
            .groups
            .get_mut(self.sel_group)
            .and_then(|g| g.sessions.get_mut(self.sel_in_group))
        {
            s.state = SessionState::Idle;
        }
        true
    }

    pub fn move_right(&mut self) {
        if let Some(group) = self.groups.get(self.sel_group) {
            if group.sessions.is_empty() {
                return;
            }
            self.sel_in_group = (self.sel_in_group + 1) % group.sessions.len();
        }
    }

    pub fn move_left(&mut self) {
        if let Some(group) = self.groups.get(self.sel_group) {
            if group.sessions.is_empty() {
                return;
            }
            if self.sel_in_group == 0 {
                self.sel_in_group = group.sessions.len() - 1;
            } else {
                self.sel_in_group -= 1;
            }
        }
    }

    pub fn move_down(&mut self) {
        if self.groups.is_empty() {
            return;
        }
        let cols = self.grid_cols as usize;
        let group = &self.groups[self.sel_group];
        let current_col = self.sel_in_group % cols;
        let next = self.sel_in_group + cols;
        if next < group.sessions.len() {
            self.sel_in_group = next;
        } else if self.sel_group + 1 < self.groups.len() {
            self.sel_group += 1;
            let new_group = &self.groups[self.sel_group];
            self.sel_in_group = current_col.min(new_group.sessions.len().saturating_sub(1));
        }
    }

    pub fn move_up(&mut self) {
        if self.groups.is_empty() {
            return;
        }
        let cols = self.grid_cols as usize;
        let current_col = self.sel_in_group % cols;
        if self.sel_in_group >= cols {
            self.sel_in_group -= cols;
        } else if self.sel_group > 0 {
            self.sel_group -= 1;
            let prev_group = &self.groups[self.sel_group];
            let last_row_start = prev_group.sessions.len().saturating_sub(1) / cols * cols;
            self.sel_in_group =
                (last_row_start + current_col).min(prev_group.sessions.len().saturating_sub(1));
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

    pub fn enter_live_tail(&mut self, view: LiveView) {
        self.live_view = Some(view);
        self.view = View::LiveTail;
    }

    pub fn close_live_tail(&mut self) {
        self.live_view = None;
        self.view = View::Grid;
    }

    pub fn enter_state_debug(&mut self) {
        self.view = View::StateDebug;
        self.state_debug = None;
        self.state_debug_lines.clear();
        self.state_debug_scroll = 0;
    }

    pub fn close_state_debug(&mut self) {
        self.view = View::Grid;
        self.state_debug = None;
        self.state_debug_lines.clear();
        self.state_debug_scroll = 0;
    }

    pub fn update_state_debug(
        &mut self,
        info: SessionInfo,
        exp: StateExplanation,
        rendered: Vec<Line<'static>>,
    ) {
        self.state_debug = Some((info, exp));
        self.state_debug_lines = rendered;
    }

    pub fn debug_scroll_down(&mut self) {
        self.state_debug_scroll = self.state_debug_scroll.saturating_add(3);
    }

    pub fn debug_scroll_up(&mut self) {
        self.state_debug_scroll = self.state_debug_scroll.saturating_sub(3);
    }

    pub fn selected_session_id(&self) -> Option<String> {
        self.selected_session_info()
            .map(|s| s.session_id.clone())
    }

    pub fn selected_session_info(&self) -> Option<&SessionInfo> {
        self.groups
            .get(self.sel_group)
            .and_then(|g| g.sessions.get(self.sel_in_group))
    }

    pub fn update_sessions(&mut self, mut sessions: Vec<SessionInfo>) {
        let prev_id = self.selected_session_id();

        let acks_active = !self.acks.is_empty();
        if acks_active {
            // Apply user acks: if a non-Idle session is still at its acked
            // watermark, downgrade it to Idle. Any advance in last_activity clears
            // the ack inside is_acked(), so the real state takes over next tick.
            for s in &mut sessions {
                if s.state != SessionState::Idle
                    && self.acks.is_acked(&s.session_id, s.last_activity)
                {
                    s.state = SessionState::Idle;
                }
            }
            let live_ids: HashSet<&str> =
                sessions.iter().map(|s| s.session_id.as_str()).collect();
            self.acks.retain_existing(&live_ids);
        }

        // Group sessions by cwd
        let mut group_map: HashMap<String, Vec<SessionInfo>> = HashMap::new();
        for s in sessions {
            group_map.entry(s.cwd.clone()).or_default().push(s);
        }

        // Scanner pre-sorts by (state, -started_at), and HashMap::entry preserves
        // bucket-relative order, so groups are already sorted unless an ack
        // downgrade changed a state above.
        if acks_active {
            for bucket in group_map.values_mut() {
                bucket.sort_by(|a, b| {
                    a.state
                        .sort_key()
                        .cmp(&b.state.sort_key())
                        .then_with(|| b.started_at.cmp(&a.started_at))
                });
            }
        }

        self.groups = group_map
            .into_iter()
            .map(|(cwd, sessions)| {
                let name = sessions
                    .first()
                    .map(|s| s.project_name.clone())
                    .unwrap_or_default();
                ProjectGroup {
                    name,
                    cwd,
                    sessions,
                }
            })
            .collect();

        // Sort groups alphabetically by name for stable ordering.
        self.groups.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        self.last_refresh = Instant::now();

        // Restore selection by session id
        if let Some(id) = prev_id {
            for (gi, group) in self.groups.iter().enumerate() {
                if let Some(si) = group.sessions.iter().position(|s| s.session_id == id) {
                    self.sel_group = gi;
                    self.sel_in_group = si;
                    return;
                }
            }
        }

        // Clamp selection
        if self.groups.is_empty() {
            self.sel_group = 0;
            self.sel_in_group = 0;
        } else {
            self.sel_group = self.sel_group.min(self.groups.len() - 1);
            let max_in = self.groups[self.sel_group]
                .sessions
                .len()
                .saturating_sub(1);
            self.sel_in_group = self.sel_in_group.min(max_in);
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

    pub fn session_count(&self) -> usize {
        self.groups.iter().map(|g| g.sessions.len()).sum()
    }

    pub fn attention_count(&self) -> usize {
        self.groups
            .iter()
            .flat_map(|g| &g.sessions)
            .filter(|s| s.needs_attention())
            .count()
    }
}
