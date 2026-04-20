use crate::acks::Acks;
use crate::conversation::StateExplanation;
use crate::folder_picker::FolderPicker;
use crate::live_view::LiveView;
use crate::metrics::MetricsAnalysis;
use crate::models::{ProjectGroup, SessionDetail, SessionInfo, SessionState};
use crate::tmux_pane::TmuxPaneView;
use crate::usage::UsageInfo;
use ratatui::text::Line;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const STATUS_MSG_TTL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq)]
pub enum View {
    Grid,
    Popup,
    LiveTail,
    ConfirmClose,
    StateDebug,
    PromptInput,
    TmuxPane,
    FolderPicker,
    GhCreateInput,
}

/// Overlay on top of [`View::FolderPicker`] that prompts for a new GitHub
/// repo name. `cwd` is captured at open time so the run target can't drift
/// if the picker is reloaded while the input is active.
#[derive(Clone, Debug)]
pub struct GhCreateInput {
    pub name: String,
    pub private: bool,
    pub cwd: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tab {
    Sessions,
    Metrics,
}

impl Tab {
    pub fn label(&self) -> &'static str {
        match self {
            Tab::Sessions => "Sessions",
            Tab::Metrics => "Metrics",
        }
    }

    pub fn cycle(&self) -> Self {
        match self {
            Tab::Sessions => Tab::Metrics,
            Tab::Metrics => Tab::Sessions,
        }
    }
}

pub const TABS: &[Tab] = &[Tab::Sessions, Tab::Metrics];

#[derive(Clone, Debug)]
pub struct PendingClose {
    pub pid: u32,
    pub display: String,
}

/// A prompt queued for a freshly-spawned tmux session that isn't yet Idle.
/// Drained by [`App::poll_pending_dispatch`] once the session shows up in the
/// next scan and its state flips to Idle, or times out after
/// [`PENDING_DISPATCH_TIMEOUT`].
#[derive(Clone, Debug)]
pub struct PendingDispatch {
    tmux: String,
    prompt: String,
    queued_at: Instant,
}

pub const PENDING_DISPATCH_TIMEOUT: Duration = Duration::from_secs(60);

pub enum DispatchAction {
    Send { tmux: String, prompt: String },
    Timeout { tmux: String },
    Wait,
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
    pub prompt_buffer: String,
    pub dispatch_target: Option<(u32, String, String)>,
    pub tmux_pane: Option<TmuxPaneView>,
    pub folder_picker: Option<FolderPicker>,
    pub gh_create_input: Option<GhCreateInput>,
    pub current_tab: Tab,
    pub metrics: Option<MetricsAnalysis>,
    pub metrics_scroll: u16,
    pub pending_dispatch: Option<PendingDispatch>,
    pub show_inactive: bool,
    /// Latest scan snapshot; drives [`rebuild_groups`].
    last_sessions: Vec<SessionInfo>,
    /// Session ids seen on the previous scan tick. `None` means the first
    /// scan hasn't happened yet — used to skip cursor-jump on initial load.
    known_session_ids: Option<HashSet<String>>,
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
            prompt_buffer: String::new(),
            dispatch_target: None,
            tmux_pane: None,
            folder_picker: None,
            gh_create_input: None,
            current_tab: Tab::Sessions,
            metrics: None,
            metrics_scroll: 0,
            pending_dispatch: None,
            show_inactive: false,
            last_sessions: Vec::new(),
            known_session_ids: None,
        }
    }

    pub fn toggle_show_inactive(&mut self) {
        self.show_inactive = !self.show_inactive;
        self.rebuild_groups();
    }

    pub fn set_tab(&mut self, tab: Tab) {
        self.current_tab = tab;
    }

    pub fn cycle_tab(&mut self) {
        self.set_tab(self.current_tab.cycle());
    }

    pub fn update_metrics(&mut self, m: MetricsAnalysis) {
        self.metrics = Some(m);
    }

    pub fn metrics_scroll_down(&mut self) {
        self.metrics_scroll = self.metrics_scroll.saturating_add(3);
    }

    pub fn metrics_scroll_up(&mut self) {
        self.metrics_scroll = self.metrics_scroll.saturating_sub(3);
    }

    pub fn enter_folder_picker(&mut self) {
        let start = self
            .selected_session_info()
            .map(|s| PathBuf::from(&s.cwd))
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        self.folder_picker = Some(FolderPicker::new(start));
        self.view = View::FolderPicker;
    }

    /// Best-guess cwd to spawn a new agent in: the selected session's cwd, or
    /// the user's home directory.
    pub fn default_spawn_cwd(&self) -> Option<String> {
        self.selected_session_info()
            .map(|s| s.cwd.clone())
            .or_else(|| dirs::home_dir().map(|p| p.display().to_string()))
    }

    pub fn close_folder_picker(&mut self) {
        self.folder_picker = None;
        self.gh_create_input = None;
        self.view = View::Grid;
    }

    /// Open the "create GitHub repo" overlay rooted in the picker's current
    /// directory. Prefills the repo name with the basename.
    pub fn enter_gh_create_input(&mut self, private: bool) {
        let Some(picker) = self.folder_picker.as_ref() else {
            return;
        };
        let cwd = picker.current_dir.display().to_string();
        let name = picker
            .current_dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.gh_create_input = Some(GhCreateInput { name, private, cwd });
        self.view = View::GhCreateInput;
    }

    pub fn close_gh_create_input(&mut self) {
        self.gh_create_input = None;
        if self.folder_picker.is_some() {
            self.view = View::FolderPicker;
        } else {
            self.view = View::Grid;
        }
    }

    pub fn submit_gh_create_input(&mut self) -> Option<(String, String, bool)> {
        let input = self.gh_create_input.take()?;
        if self.folder_picker.is_some() {
            self.view = View::FolderPicker;
        } else {
            self.view = View::Grid;
        }
        Some((input.cwd, input.name, input.private))
    }

    pub fn enter_tmux_pane(&mut self, view: TmuxPaneView) {
        self.tmux_pane = Some(view);
        self.view = View::TmuxPane;
    }

    pub fn close_tmux_pane(&mut self) {
        self.tmux_pane = None;
        self.view = View::Grid;
    }

    pub fn enter_prompt_input(&mut self) {
        self.prompt_buffer.clear();
        self.dispatch_target = Self::compute_dispatch_target(&self.groups);
        self.view = View::PromptInput;
    }

    pub fn close_prompt_input(&mut self) {
        self.prompt_buffer.clear();
        self.dispatch_target = None;
        self.view = View::Grid;
    }

    pub fn submit_prompt_input(&mut self) -> String {
        self.view = View::Grid;
        std::mem::take(&mut self.prompt_buffer)
    }

    pub fn dispatch_target(&self) -> Option<&(u32, String, String)> {
        self.dispatch_target.as_ref()
    }

    pub fn queue_pending_dispatch(&mut self, tmux: String, prompt: String) {
        self.pending_dispatch = Some(PendingDispatch {
            tmux,
            prompt,
            queued_at: Instant::now(),
        });
    }

    pub fn has_pending_dispatch(&self) -> bool {
        self.pending_dispatch.is_some()
    }

    /// If a pending dispatch exists and the target session now reports Idle,
    /// consume it and return [`DispatchAction::Send`]. If the deadline has
    /// passed, return [`DispatchAction::Timeout`]. Otherwise, put it back and
    /// wait.
    pub fn poll_pending_dispatch(&mut self) -> DispatchAction {
        let Some(pd) = self.pending_dispatch.take() else {
            return DispatchAction::Wait;
        };
        let ready = self.groups.iter().flat_map(|g| &g.sessions).any(|s| {
            s.tmux_session.as_deref() == Some(pd.tmux.as_str())
                && s.state == SessionState::Idle
        });
        if ready {
            return DispatchAction::Send { tmux: pd.tmux, prompt: pd.prompt };
        }
        if pd.queued_at.elapsed() > PENDING_DISPATCH_TIMEOUT {
            return DispatchAction::Timeout { tmux: pd.tmux };
        }
        self.pending_dispatch = Some(pd);
        DispatchAction::Wait
    }

    fn compute_dispatch_target(
        groups: &[crate::models::ProjectGroup],
    ) -> Option<(u32, String, String)> {
        let panes = crate::send::tmux_panes();
        groups
            .iter()
            .flat_map(|g| &g.sessions)
            .filter(|s| s.state == SessionState::Idle)
            .filter_map(|s| {
                let tmux = crate::send::tmux_session_for_pid_in(s.pid, &panes)?;
                Some((s, tmux))
            })
            .max_by_key(|(s, _)| s.last_activity.unwrap_or(s.started_at))
            .map(|(s, tmux)| (s.pid, s.project_name.clone(), tmux))
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
        let acks_active = !self.acks.is_empty();
        if acks_active {
            // Apply user acks: if a non-Idle session is still at its acked
            // watermark, downgrade it to Idle. Any advance in last_activity clears
            // the ack inside is_acked(), so the real state takes over next tick.
            for s in &mut sessions {
                if s.state != SessionState::Idle
                    && s.state != SessionState::Inactive
                    && self.acks.is_acked(&s.session_id, s.last_activity)
                {
                    s.state = SessionState::Idle;
                }
            }
            let live_ids: HashSet<&str> =
                sessions.iter().map(|s| s.session_id.as_str()).collect();
            self.acks.retain_existing(&live_ids);
        }

        self.last_sessions = sessions;
        self.rebuild_groups();
        self.last_refresh = Instant::now();

        let current_ids: HashSet<String> = self
            .groups
            .iter()
            .flat_map(|g| g.sessions.iter().map(|s| s.session_id.clone()))
            .collect();
        // First tick seeds known ids without hijacking the cursor; later ticks
        // jump selection to a freshly-appeared session so it gets focus.
        let new_selection = self.known_session_ids.as_ref().and_then(|known| {
            self.groups.iter().enumerate().find_map(|(gi, group)| {
                group
                    .sessions
                    .iter()
                    .position(|s| !known.contains(&s.session_id))
                    .map(|si| (gi, si))
            })
        });
        self.known_session_ids = Some(current_ids);
        if let Some((gi, si)) = new_selection {
            self.sel_group = gi;
            self.sel_in_group = si;
        }
    }

    fn rebuild_groups(&mut self) {
        let prev_id = self.selected_session_id();
        let acks_active = !self.acks.is_empty();

        let sessions: Vec<SessionInfo> = self
            .last_sessions
            .iter()
            .filter(|s| self.show_inactive || s.state != SessionState::Inactive)
            .cloned()
            .collect();

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

        if self.view == View::PromptInput {
            self.dispatch_target = Self::compute_dispatch_target(&self.groups);
        }

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

    pub fn log_state_dump(&self) {
        log::info!("=== state dump on quit ===");
        log::info!(
            "view={:?} sel_group={} sel_in_group={} grid_cols={} groups={} sessions={} attention={}",
            self.view,
            self.sel_group,
            self.sel_in_group,
            self.grid_cols,
            self.groups.len(),
            self.session_count(),
            self.attention_count()
        );
        if let Some(sel) = self.selected_session_info() {
            log::info!(
                "selected: pid={} sid={} project={} state={}",
                sel.pid,
                crate::models::short_sid(&sel.session_id),
                sel.project_name,
                sel.state
            );
        }
        if let Some(u) = &self.usage {
            log::info!("usage: {:?}", u);
        }
        if let Some((msg, _)) = &self.status_msg {
            log::info!("status_msg: {}", msg);
        }
        if let Some(pc) = &self.pending_close {
            log::info!("pending_close: pid={} display={}", pc.pid, pc.display);
        }
        if let Some((target_pid, name, tmux)) = &self.dispatch_target {
            log::info!(
                "dispatch_target: pid={} project={} tmux={}",
                target_pid, name, tmux
            );
        }
        if !self.acks.is_empty() {
            log::info!("acks: active");
        }
        for (gi, group) in self.groups.iter().enumerate() {
            log::info!(
                "group[{}]: name={} cwd={} sessions={}",
                gi,
                group.name,
                group.cwd,
                group.sessions.len()
            );
            for (si, s) in group.sessions.iter().enumerate() {
                log::info!(
                    "  session[{}]: pid={} sid={} state={} started_at={} last_activity={:?} model={:?} branch={:?} version={:?} tmux={:?} last_msg={:?}",
                    si,
                    s.pid,
                    crate::models::short_sid(&s.session_id),
                    s.state,
                    s.started_at,
                    s.last_activity,
                    s.model,
                    s.git_branch,
                    s.version,
                    s.tmux_session,
                    s.last_user_message.as_deref().map(|m| {
                        let trimmed: String = m.chars().take(80).collect();
                        trimmed
                    })
                );
            }
        }
        log::info!("=== end state dump ===");
    }
}
