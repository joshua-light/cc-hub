//! The TUI view-model. [`App`] holds every tab's state, the active overlay
//! and its modal state; `ui/` renders it and `bin/` drives it through
//! [`Command`]s.
//!
//! - `view`: the [`View`] and [`Tab`] enums and the generic overlays.
//! - `command`: the [`Command`] dispatcher and the [`Effect`]s it asks the
//!   event loop to run.
//! - `sessions`: the Sessions tab (grid, scan pipeline, spawn watchdogs,
//!   rename, and the pickers opened from a session card).
//! - `board`: the Tasks tab (column order, card lifecycle, popups,
//!   agent assignment).
//! - `places`: the folder / places / bookmarks picker.
//! - `dispatch`: prompts queued for sessions that are still booting.
//! - `builds_view`, `harness_view`, `metrics_view`: the Builds, Agents and
//!   Metrics tabs.
//! - `render_state`: layout state the renderer writes back.

use crate::agent_runtime::{AgentRuntime, SystemAgentRuntime};
use crate::bookmarks::Bookmarks;
use crate::config;
use crate::folder_picker::FolderPicker;
use crate::live_view::LiveView;
use crate::models::{SessionDetail, SessionInfo};
use crate::sessions::count::SessionCounts;
use crate::tmux_pane::TmuxPaneView;
use crate::usage::UsageInfo;
use ratatui::text::Line;
use sessions::SpawnWatch;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod board;
mod builds_view;
mod command;
mod dispatch;
mod harness_view;
mod metrics_view;
mod picker_list;
mod places;
mod render_state;
mod sessions;
#[cfg(test)]
mod test_support;
mod view;

pub use board::{
    column_statuses, visible_task_columns, TaskField, TaskKindPickerState, TasksView,
    PROCEED_PROMPT, TASK_COLUMNS,
};
pub use builds_view::{BuildForm, BuildsProbe, BuildsSnapshot, BuildsView, FormField, LogView};
pub use command::{
    BuildsCommand, Command, Effect, GlobalCommand, HarnessCommand, SessionsCommand, TasksCommand,
};
pub use dispatch::{DispatchAction, PendingDispatch};
pub use harness_view::{Detail, HarnessView, Section};
pub use metrics_view::MetricsView;
pub use picker_list::PickerRow;
pub use places::GhCreateInput;
pub use render_state::RenderState;
pub use sessions::{
    AgentPickerState, ModelPickerChoice, ModelPickerRow, ModelPickerState, RenameSubmit,
    RespawnChoice, RespawnPickerState, SessionFinderChoice, SessionFinderRow, SessionFinderState,
    SessionsLayout, SessionsView, TaskLinkAction, TaskLinkChoice, TaskLinkPickerState, TaskLinkRow,
    SPAWN_MODELS,
};
pub use view::{visible_tabs, PendingClose, PendingConfirm, Tab, View, TABS};

pub fn status_msg_ttl() -> Duration {
    config::get().ui.status_msg_ttl()
}

pub struct App {
    runtime: Arc<dyn AgentRuntime>,
    pub sessions: SessionsView,
    pub metrics: MetricsView,
    pub harness: HarnessView,
    pub builds: BuildsView,
    pub tasks: TasksView,
    pub view: View,
    pub detail: Option<SessionDetail>,
    pub detail_loading: bool,
    /// Layout state written by `ui/` during draw (scroll clamps, grid geometry,
    /// decoded-image cache). The renderer owns these; nav methods only read and
    /// adjust via named methods. See [`RenderState`].
    pub render: RenderState,
    pub should_quit: bool,
    pub last_refresh: Instant,
    pub live_view: Option<LiveView>,
    pub status_msg: Option<(String, Instant)>,
    /// The single staged destructive/interrupting action behind
    /// [`View::ConfirmClose`]. At most one can be pending at a time, which
    /// the [`PendingConfirm`] enum makes structural rather than conventional.
    pub pending_confirm: Option<PendingConfirm>,
    pub usage: Option<UsageInfo>,
    pub usage_line: Line<'static>,
    pub session_counts: SessionCounts,
    /// In-progress edit buffer for the rename-session prompt (`r` on the
    /// Sessions tab). Prefilled with the current title so the user edits
    /// rather than retypes.
    pub rename_buffer: String,
    /// `session_id` being renamed while [`View::RenameSession`] is open, so
    /// the submit can target the right session even if the selection moves
    /// underneath the modal on a rescan.
    pub rename_target: Option<String>,
    pub model_picker: Option<ModelPickerState>,
    pub agent_picker: Option<AgentPickerState>,
    /// State behind [`View::RespawnPicker`] (`R` on the Sessions tab).
    pub respawn_picker: Option<RespawnPickerState>,
    pub task_kind_picker: Option<TaskKindPickerState>,
    /// Agent used by Sessions-tab new-session actions. Seeded from
    /// `[projects].default_session_agent`; `A` changes it for this run.
    default_session_agent_id: String,
    /// State behind [`View::TaskLinkPicker`] (`L` on the Sessions tab).
    pub task_link_picker: Option<TaskLinkPickerState>,
    /// State behind [`View::SessionFinder`] (`/` on the Sessions tab).
    pub session_finder: Option<SessionFinderState>,
    /// `session_id → TaskLink` sidecar snapshot driving the `project ▸ task`
    /// grouping in [`Self::build_groups`]. Reloaded from disk on every scan
    /// tick (mirroring how the scanner re-reads the title sidecar) so links
    /// written by another instance show up without a restart.
    pub(crate) session_task_links: HashMap<String, crate::tasks::session_links::TaskLink>,
    pub tmux_pane: Option<TmuxPaneView>,
    pub folder_picker: Option<FolderPicker>,
    /// Persistent folder bookmarks shown by the bookmarks picker (`M`) and
    /// queried while rendering the regular picker so already-bookmarked
    /// entries get a marker. Loaded once on startup.
    pub bookmarks: Bookmarks,
    pub gh_create_input: Option<GhCreateInput>,
    pub current_tab: Tab,
    pub pending_dispatch: VecDeque<PendingDispatch>,
    /// Last time [`Self::poll_pending_dispatch`] ran the `pane_ready_for_input`
    /// probe (a `tmux capture-pane` fork+exec). The poll is called every render
    /// frame (~50ms) while a dispatch is pending, but the pane only needs
    /// checking a couple of times a second — this throttles the fork+exec so
    /// the event loop isn't spending most frames waiting on tmux.
    last_dispatch_probe_at: Option<Instant>,
    /// Watchdogs for user-initiated detached spawns ('n', folder picker).
    /// A spawn can succeed at the tmux level yet never start the agent —
    /// e.g. a shell-rc prompt blocking the detached pane — and without a
    /// watchdog that failure is invisible: the status bar said "started" and
    /// no card ever appears. Checked against each scan snapshot; see
    /// [`Self::check_spawn_watches`].
    spawn_watches: Vec<SpawnWatch>,
    /// Boot-time name prompts, keyed by the spawning tmux name — the stable
    /// bridge across a session's placeholder→real transition, since its real
    /// scanner id isn't known until it loads. A present key means this spawn
    /// already got its rename popup, so the load-time
    /// [`Self::maybe_autoprompt_rename_for_new_session`] stays quiet for it.
    /// `Some(title)` means the user named it while it was still booting; the
    /// title is persisted to the real session id the moment the scanner sees
    /// it (see [`Self::adopt_pending_spawn_names`]). Populated while Haiku
    /// auto-titling is off, and by respawns carrying the old session's name.
    pending_spawn_names: HashMap<String, Option<String>>,
    /// The card a Done was just refused on, because its newest note still
    /// asks the user something. A second Done on that same card goes
    /// through — the board is the user's, and a card can be abandoned on
    /// purpose; what it can no longer be is closed by accident with an
    /// agent's question unanswered. See [`Self::unanswered_ask`].
    done_refused_on: Option<String>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self::new_with_runtime(Arc::new(SystemAgentRuntime))
    }

    pub fn new_with_runtime(runtime: Arc<dyn AgentRuntime>) -> Self {
        Self {
            runtime,
            sessions: SessionsView::new(),
            metrics: MetricsView::new(),
            harness: HarnessView::default(),
            builds: BuildsView::default(),
            tasks: TasksView::new(),
            view: View::Grid,
            detail: None,
            detail_loading: false,
            render: RenderState::default(),
            should_quit: false,
            last_refresh: Instant::now(),
            live_view: None,
            status_msg: None,
            pending_confirm: None,
            usage: None,
            usage_line: Line::default(),
            session_counts: SessionCounts::default(),
            rename_buffer: String::new(),
            rename_target: None,
            model_picker: None,
            agent_picker: None,
            respawn_picker: None,
            task_kind_picker: None,
            default_session_agent_id: config::get().default_session_agent_id(),
            task_link_picker: None,
            session_finder: None,
            session_task_links: crate::tasks::session_links::load(),
            tmux_pane: None,
            folder_picker: None,
            bookmarks: Bookmarks::load(),
            gh_create_input: None,
            current_tab: Tab::Sessions,
            pending_dispatch: VecDeque::new(),
            last_dispatch_probe_at: None,
            spawn_watches: Vec::new(),
            pending_spawn_names: HashMap::new(),
            done_refused_on: None,
        }
    }

    pub fn update_usage(&mut self, usage: UsageInfo, rendered: Line<'static>) {
        self.usage = Some(usage);
        self.usage_line = rendered;
    }

    pub fn update_session_counts(&mut self, counts: SessionCounts) {
        self.session_counts = counts;
    }

    pub fn set_status(&mut self, msg: String) {
        self.status_msg = Some((msg, Instant::now()));
    }

    pub fn selected_session_id(&self) -> Option<String> {
        self.sessions.selected_session_id()
    }

    pub fn selected_session_info(&self) -> Option<&SessionInfo> {
        self.sessions.selected_session_info()
    }

    pub fn update_detail(&mut self, detail: SessionDetail) {
        self.detail = Some(detail);
        self.detail_loading = false;
    }

    pub fn update_grid_cols(&mut self, width: u16) {
        let cell_width = config::get().ui.cell_width.max(1);
        self.render.grid_cols = (width / cell_width).max(1);
    }

    pub fn session_count(&self) -> usize {
        self.sessions.session_count()
    }

    pub fn attention_count(&self) -> usize {
        self.sessions.attention_count() + self.harness.attention_count()
    }

    /// Replace the Agents-tab snapshot (from the periodic disk scan).
    pub fn update_harness(&mut self, agents: Vec<crate::harness::AgentSnapshot>) {
        self.harness.update(agents);
    }

    /// Replace the Builds-tab snapshot (from the one-second disk read).
    pub fn update_builds(&mut self, snapshot: BuildsSnapshot) {
        self.builds.update(snapshot);
    }

    pub fn update_builds_probe(&mut self, probe: BuildsProbe) {
        self.builds.probe = probe;
    }

    pub fn log_state_dump(&self) {
        log::info!("=== state dump on quit ===");
        log::info!(
            "view={:?} sel_group={} sel_in_group={} grid_cols={} groups={} sessions={} attention={}",
            self.view,
            self.sessions.sel_group,
            self.sessions.sel_in_group,
            self.render.grid_cols,
            self.sessions.groups.len(),
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
        if let Some(PendingConfirm::Close(pc)) = &self.pending_confirm {
            log::info!("pending_close: pid={} display={}", pc.pid, pc.display);
        }
        if !self.sessions.acks.is_empty() {
            log::info!("acks: active");
        }
        for (gi, group) in self.sessions.groups.iter().enumerate() {
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
