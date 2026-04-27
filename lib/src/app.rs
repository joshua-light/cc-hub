use crate::acks::Acks;
use crate::config;
use crate::conversation::StateExplanation;
use crate::folder_picker::FolderPicker;
use crate::live_view::LiveView;
use crate::metrics::{MetricsAnalysis, SelectableSession};
use crate::models::{ProjectGroup, SessionDetail, SessionInfo, SessionState};
use crate::projects_scan::ProjectsSnapshot;
use crate::tmux_pane::TmuxPaneView;
use crate::usage::UsageInfo;
use ratatui::text::Line;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub fn status_msg_ttl() -> Duration {
    config::get().ui.status_msg_ttl()
}

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
    ProjectsResult,
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
    Projects,
    Sessions,
    Metrics,
}

impl Tab {
    pub fn label(&self) -> &'static str {
        match self {
            Tab::Projects => "Projects",
            Tab::Sessions => "Sessions",
            Tab::Metrics => "Metrics",
        }
    }

    pub fn cycle(&self) -> Self {
        match self {
            Tab::Projects => Tab::Sessions,
            Tab::Sessions => Tab::Metrics,
            Tab::Metrics => Tab::Projects,
        }
    }
}

pub const TABS: &[Tab] = &[Tab::Projects, Tab::Sessions, Tab::Metrics];

#[derive(Clone, Debug)]
pub struct PendingClose {
    pub pid: u32,
    pub display: String,
}

/// Pending project-task deletion. Shown via the same `ConfirmClose` view
/// as session close, distinguished by [`App::pending_task_delete`] being
/// `Some` (vs. [`App::pending_close`]).
#[derive(Clone, Debug)]
pub struct PendingTaskDelete {
    pub project_id: String,
    pub task_id: String,
    pub display: String,
    /// tmux name of the orchestrator, captured at delete-prompt time so a
    /// concurrent state rewrite can't change what we kill.
    pub orchestrator_tmux: Option<String>,
}

/// A prompt queued for a freshly-spawned tmux session that isn't yet Idle.
/// Drained by [`App::poll_pending_dispatch`] once the session shows up in the
/// next scan and its state flips to Idle, or times out after
/// [`config::UiConfig::pending_dispatch_timeout_secs`].
#[derive(Clone, Debug)]
pub struct PendingDispatch {
    tmux: String,
    prompt: String,
    queued_at: Instant,
}

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
    pub pending_task_delete: Option<PendingTaskDelete>,
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
    pub metrics_rows: Vec<SelectableSession>,
    pub metrics_selected: Option<usize>,
    /// (scanned, total) for the in-flight metrics scan, shown while
    /// [`Self::metrics`] is `None`. Cleared once analysis completes.
    pub metrics_progress: Option<(usize, usize)>,
    pub pending_dispatch: Option<PendingDispatch>,
    pub show_inactive: bool,
    pub projects: ProjectsSnapshot,
    /// Cursor in the Projects tab. `0..projects.len()` selects a project;
    /// task selection within the project lives in [`Self::projects_task_sel`].
    pub projects_sel: usize,
    pub projects_task_sel: usize,
    /// True while the folder picker / prompt input flow is creating a
    /// new project task (vs. spawning a regular session). Used to route
    /// the picker's space-pick and prompt-input's enter to the project
    /// flow instead of [`spawn::spawn_claude_session`].
    pub creating_project_task: bool,
    /// cwd captured when the picker chose a folder in Projects mode. Held
    /// until the user submits the task prompt; consumed in
    /// [`Self::submit_project_task`].
    pub projects_pending_cwd: Option<String>,
    /// Latest scan snapshot; drives [`rebuild_groups`].
    last_sessions: Vec<SessionInfo>,
    /// Session ids seen on the previous scan tick. `None` means the first
    /// scan hasn't happened yet — used to skip cursor-jump on initial load.
    known_session_ids: Option<HashSet<String>>,
    /// Cursor inside the Projects "Result" popup, indexing into the
    /// selected task's `artifacts` vec. Reset on popup open.
    pub result_artifact_sel: usize,
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
            pending_task_delete: None,
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
            metrics_rows: Vec::new(),
            metrics_selected: None,
            metrics_progress: None,
            pending_dispatch: None,
            show_inactive: false,
            projects: ProjectsSnapshot::empty(),
            projects_sel: 0,
            projects_task_sel: 0,
            creating_project_task: false,
            projects_pending_cwd: None,
            last_sessions: Vec::new(),
            known_session_ids: None,
            result_artifact_sel: 0,
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

    pub fn update_projects(&mut self, snap: ProjectsSnapshot) {
        // Preserve cursor when possible: keep the same project_id selected
        // across rescans even if the order shifted.
        let prev_pid = self
            .projects
            .projects
            .get(self.projects_sel)
            .map(|p| p.id.clone());
        self.projects = snap;
        if let Some(pid) = prev_pid {
            if let Some(idx) = self.projects.projects.iter().position(|p| p.id == pid) {
                self.projects_sel = idx;
            }
        }
        self.clamp_projects_cursor();
    }

    fn clamp_projects_cursor(&mut self) {
        let n = self.projects.projects.len();
        if n == 0 {
            self.projects_sel = 0;
            self.projects_task_sel = 0;
            return;
        }
        if self.projects_sel >= n {
            self.projects_sel = n - 1;
        }
        let task_count = self
            .projects
            .projects
            .get(self.projects_sel)
            .and_then(|p| self.projects.tasks.get(&p.id))
            .map(|v| v.len())
            .unwrap_or(0);
        if task_count == 0 {
            self.projects_task_sel = 0;
        } else if self.projects_task_sel >= task_count {
            self.projects_task_sel = task_count - 1;
        }
    }

    pub fn projects_move_down(&mut self) {
        if self.projects.projects.is_empty() {
            return;
        }
        self.projects_sel =
            (self.projects_sel + 1).min(self.projects.projects.len() - 1);
        self.projects_task_sel = 0;
    }

    pub fn projects_move_up(&mut self) {
        self.projects_sel = self.projects_sel.saturating_sub(1);
        self.projects_task_sel = 0;
    }

    pub fn projects_task_next(&mut self) {
        let task_count = self
            .projects
            .projects
            .get(self.projects_sel)
            .and_then(|p| self.projects.tasks.get(&p.id))
            .map(|v| v.len())
            .unwrap_or(0);
        if task_count == 0 {
            return;
        }
        self.projects_task_sel =
            (self.projects_task_sel + 1).min(task_count - 1);
    }

    pub fn projects_task_prev(&mut self) {
        self.projects_task_sel = self.projects_task_sel.saturating_sub(1);
    }

    pub fn selected_project(&self) -> Option<&crate::orchestrator::Project> {
        self.projects.projects.get(self.projects_sel)
    }

    pub fn selected_project_task(&self) -> Option<&crate::orchestrator::TaskState> {
        let p = self.selected_project()?;
        self.projects
            .tasks
            .get(&p.id)
            .and_then(|v| v.get(self.projects_task_sel))
    }

    pub fn sessions_by_tmux(&self) -> HashMap<&str, &SessionInfo> {
        let mut out = HashMap::new();
        for s in &self.last_sessions {
            if let Some(name) = s.tmux_session.as_deref() {
                out.insert(name, s);
            }
        }
        out
    }

    pub fn update_metrics(&mut self, m: MetricsAnalysis) {
        let prev_sid = self
            .metrics_selected
            .and_then(|i| self.metrics_rows.get(i))
            .map(|r| r.session_id.clone());
        self.metrics_rows = m.selectable_sessions();
        self.metrics = Some(m);
        self.metrics_progress = None;
        self.metrics_selected = match prev_sid {
            Some(sid) => self
                .metrics_rows
                .iter()
                .position(|r| r.session_id == sid)
                .or_else(|| (!self.metrics_rows.is_empty()).then_some(0)),
            None => (!self.metrics_rows.is_empty()).then_some(0),
        };
    }

    pub fn update_metrics_progress(&mut self, scanned: usize, total: usize) {
        if self.metrics.is_some() {
            return;
        }
        self.metrics_progress = Some((scanned, total));
    }

    pub fn metrics_scroll_down(&mut self) {
        self.metrics_scroll = self.metrics_scroll.saturating_add(3);
    }

    pub fn metrics_scroll_up(&mut self) {
        self.metrics_scroll = self.metrics_scroll.saturating_sub(3);
    }

    pub fn metrics_sel_next(&mut self) {
        if self.metrics_rows.is_empty() {
            self.metrics_selected = None;
            return;
        }
        let next = match self.metrics_selected {
            Some(i) => (i + 1).min(self.metrics_rows.len() - 1),
            None => 0,
        };
        self.metrics_selected = Some(next);
    }

    pub fn metrics_sel_prev(&mut self) {
        if self.metrics_rows.is_empty() {
            self.metrics_selected = None;
            return;
        }
        let prev = match self.metrics_selected {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.metrics_selected = Some(prev);
    }

    pub fn selected_metrics_session(&self) -> Option<&SelectableSession> {
        self.metrics_selected
            .and_then(|i| self.metrics_rows.get(i))
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

    /// Open the folder picker rooted at the most useful starting point for
    /// project creation: the selected project's root if any, else $HOME.
    /// Sets [`Self::creating_project_task`] so picker-pick routes through
    /// the orchestrator flow.
    pub fn enter_folder_picker_for_projects(&mut self) {
        let start = self
            .selected_project()
            .map(|p| p.root.clone())
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        self.folder_picker = Some(FolderPicker::new(start));
        self.creating_project_task = true;
        self.view = View::FolderPicker;
    }

    /// Picker chose `cwd` while in projects-creation mode. Stash the cwd
    /// and switch to a multi-line prompt input; the actual orchestrator
    /// spawn happens in [`Self::submit_project_task`].
    pub fn enter_project_task_prompt(&mut self, cwd: String) {
        self.folder_picker = None;
        self.projects_pending_cwd = Some(cwd);
        self.prompt_buffer.clear();
        self.dispatch_target = None;
        self.view = View::PromptInput;
    }

    /// Shortcut for "new task on the currently-selected project" — same
    /// as [`Self::enter_project_task_prompt`] but skips the folder picker
    /// by reusing the selected project's stored root. Returns false (and
    /// no-ops) if no project is selected.
    pub fn enter_project_task_prompt_for_selected(&mut self) -> bool {
        let Some(project) = self.selected_project().cloned() else {
            return false;
        };
        let cwd = project.root.display().to_string();
        self.enter_project_task_prompt(cwd);
        true
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
        self.creating_project_task = false;
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
        self.projects_pending_cwd = None;
        self.creating_project_task = false;
        self.view = View::Grid;
    }

    /// True when the prompt input should be routed through the orchestrator
    /// project-task flow instead of the regular session-dispatch flow.
    pub fn prompt_input_for_project(&self) -> bool {
        self.projects_pending_cwd.is_some()
    }

    /// Consumes the pending cwd, clears prompt input, returns the
    /// `(cwd, prompt)` pair. Returns `None` if either is missing.
    pub fn submit_project_task(&mut self) -> Option<(String, String)> {
        let cwd = self.projects_pending_cwd.take()?;
        self.creating_project_task = false;
        self.view = View::Grid;
        let prompt = std::mem::take(&mut self.prompt_buffer);
        Some((cwd, prompt))
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
        // Layered readiness, in order of preference:
        //   1. scanner says Idle AND pane shows claude's empty input row.
        //      Tightest gate — guarantees the next paste lands in the
        //      right place. Preferred when both signals agree.
        //   2. scanner says Idle AND we've waited long enough for cold
        //      boot (>5s). Fallback for the case where the pane-ready
        //      check stays false because claude is rendering something
        //      we don't recognise (different glyph, different theme).
        //      Without this, a single cosmetic mismatch loses the prompt
        //      to the timeout and the user sees a "session that just
        //      sits there empty" — the real-world failure mode that
        //      motivated this comment.
        let scanner_idle = self.groups.iter().flat_map(|g| &g.sessions).any(|s| {
            s.tmux_session.as_deref() == Some(pd.tmux.as_str())
                && s.state == SessionState::Idle
        });
        if scanner_idle {
            let pane_ready = crate::send::pane_ready_for_input(&pd.tmux);
            let aged_in = pd.queued_at.elapsed() >= Duration::from_secs(5);
            if pane_ready || aged_in {
                if !pane_ready {
                    log::info!(
                        "dispatch: pane_ready=false but {}s elapsed — sending anyway (target=[{}])",
                        pd.queued_at.elapsed().as_secs(),
                        pd.tmux
                    );
                }
                return DispatchAction::Send { tmux: pd.tmux, prompt: pd.prompt };
            }
        }
        if pd.queued_at.elapsed() > config::get().ui.pending_dispatch_timeout() {
            return DispatchAction::Timeout { tmux: pd.tmux };
        }
        self.pending_dispatch = Some(pd);
        DispatchAction::Wait
    }

    /// Time the current pending dispatch has been waiting. None when no
    /// dispatch is queued. Used by the status bar so the user can tell
    /// at a glance that an orchestrator/dispatch is still booting rather
    /// than wondering why nothing is happening.
    pub fn pending_dispatch_age(&self) -> Option<Duration> {
        self.pending_dispatch.as_ref().map(|pd| pd.queued_at.elapsed())
    }

    /// Tmux session name of the current pending dispatch, if any.
    pub fn pending_dispatch_target(&self) -> Option<&str> {
        self.pending_dispatch.as_ref().map(|pd| pd.tmux.as_str())
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
        self.pending_task_delete = None;
        self.view = View::Grid;
    }

    pub fn take_pending_close(&mut self) -> Option<PendingClose> {
        self.view = View::Grid;
        self.pending_close.take()
    }

    /// Stage a project-task deletion behind the same ConfirmClose flow used
    /// for sessions. Resolves `orchestrator_tmux` synchronously so the kill
    /// step doesn't have to re-load state.json.
    pub fn enter_confirm_task_delete(&mut self) {
        let Some(p) = self.selected_project().cloned() else { return };
        let Some(task) = self.selected_project_task().cloned() else { return };
        let status_label = match task.status {
            crate::orchestrator::TaskStatus::Running => "running",
            crate::orchestrator::TaskStatus::Done => "done",
            crate::orchestrator::TaskStatus::Failed => "failed",
        };
        let display = format!(
            "{} — {} (task {})",
            p.name,
            status_label,
            crate::orchestrator::short_task_id(&task.task_id),
        );
        self.pending_task_delete = Some(PendingTaskDelete {
            project_id: p.id.clone(),
            task_id: task.task_id.clone(),
            display,
            orchestrator_tmux: task.orchestrator_tmux.clone(),
        });
        self.view = View::ConfirmClose;
    }

    pub fn take_pending_task_delete(&mut self) -> Option<PendingTaskDelete> {
        self.view = View::Grid;
        self.pending_task_delete.take()
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
        let cell_width = config::get().ui.cell_width.max(1);
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

    /// Open the Projects "Result" popup for the currently-selected task.
    /// Returns false when no task is selected, so the caller can surface a
    /// status-bar hint instead of opening an empty popup.
    pub fn enter_projects_result(&mut self) -> bool {
        if self.selected_project_task().is_none() {
            return false;
        }
        self.result_artifact_sel = 0;
        self.view = View::ProjectsResult;
        true
    }

    pub fn close_projects_result(&mut self) {
        self.view = View::Grid;
        self.result_artifact_sel = 0;
    }

    pub fn result_artifact_next(&mut self) {
        let n = self
            .selected_project_task()
            .map(|t| t.artifacts.len())
            .unwrap_or(0);
        if n == 0 {
            self.result_artifact_sel = 0;
            return;
        }
        self.result_artifact_sel = (self.result_artifact_sel + 1).min(n - 1);
    }

    pub fn result_artifact_prev(&mut self) {
        self.result_artifact_sel = self.result_artifact_sel.saturating_sub(1);
    }

    /// The artifact under the popup cursor, if any. Used by the `c` and `o`
    /// keybinds to know what path to act on.
    pub fn selected_result_artifact(&self) -> Option<&crate::orchestrator::Artifact> {
        let t = self.selected_project_task()?;
        t.artifacts.get(self.result_artifact_sel)
    }
}

