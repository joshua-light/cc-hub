use crate::bookmarks::Bookmarks;
use crate::config;
use crate::conversation::StateExplanation;
use crate::folder_picker::{FolderPicker, PickerMode, Place};
use crate::live_view::LiveView;
use crate::metrics::{MetricsAnalysis, SelectableSession};
use crate::models::{ProjectGroup, SessionDetail, SessionInfo, SessionState};
use crate::projects_scan::ProjectsSnapshot;
use crate::session_count::SessionCounts;
use crate::tasks::{TaskItem, TaskItemStatus, TaskPriority};
use crate::tmux_pane::TmuxPaneView;
use crate::usage::UsageInfo;
use ratatui::text::Line;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

mod metrics_view;
mod projects_view;
mod sessions_view;
mod tasks_view;
mod todo_panel;

pub use metrics_view::MetricsView;
pub use projects_view::ProjectsView;
pub use sessions_view::SessionsView;
pub use tasks_view::{column_statuses, visible_task_columns, TasksView, TASK_COLUMNS};
pub use todo_panel::TodoPanelState;

pub fn status_msg_ttl() -> Duration {
    config::get().ui.status_msg_ttl()
}

/// What Space on a Planning card sends to the bound agent. Kept terse: the
/// plan-first framing in [`planning_prompt`] already told the agent what
/// "proceed" means.
pub const PROCEED_PROMPT: &str = "Proceed with the implementation.";

/// Wrap a board task's text in plan-first framing: the agent investigates
/// and presents a plan, then holds until the user approves (Space sends
/// [`PROCEED_PROMPT`]). This is what makes the Planning column honest — the
/// agent genuinely isn't implementing while the card sits there.
fn planning_prompt(text: &str) -> String {
    format!(
        "{text}\n\nFirst investigate the codebase and figure out what this task needs, \
         then present a short implementation plan: approach, files you'll touch, open \
         questions. Do NOT implement yet — stop after the plan and wait for me to say \
         \"proceed\"."
    )
}

#[derive(Clone, Debug, PartialEq)]
pub enum View {
    Grid,
    Popup,
    LiveTail,
    ConfirmClose,
    StateDebug,
    PromptInput,
    RenameSession,
    TmuxPane,
    FolderPicker,
    GhCreateInput,
    ProjectsResult,
    Backlog,
    /// Scratch to-do side panel on the Sessions tab (toggled with `t`).
    TodoPanel,
    /// Centered single-line input for adding a task on the Tasks tab.
    TaskInput,
    /// Centered single-line input for editing the focused task's tags.
    TaskTags,
}

/// Outcome of pressing Space on a focused Projects-tab task. The caller
/// uses this to decide whether to show the generic "nothing to approve"
/// toast and whether to notify the orchestrator tmux to continue the
/// merge flow. Specific failure/success messaging is handled inside
/// `approve_review_task` via `set_status`; the caller only acts on the
/// variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApproveOutcome {
    /// No focused Review task — caller should show the generic toast.
    NotReviewTask,
    /// PR was approved; caller should ping the live orchestrator tmux.
    PrApproved,
    /// Review task without a PR was transitioned to Done; status set.
    DoneNoPr,
    /// Approve attempted but failed; specific reason already in status.
    Failed,
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
    Tasks,
    Projects,
    Sessions,
    Metrics,
}

impl Tab {
    pub fn label(&self) -> &'static str {
        match self {
            Tab::Tasks => "Tasks",
            Tab::Projects => "Projects",
            Tab::Sessions => "Sessions",
            Tab::Metrics => "Metrics",
        }
    }
}

pub const TABS: &[Tab] = &[Tab::Tasks, Tab::Projects, Tab::Sessions, Tab::Metrics];

/// Tabs shown in the strip and reachable via ⇥, in [`TABS`] order. The
/// Projects tab is WIP-gated behind `[ui] show_projects_tab` (its scan and
/// data stay live either way — only the tab is hidden).
pub fn visible_tabs() -> Vec<Tab> {
    TABS.iter()
        .copied()
        .filter(|t| *t != Tab::Projects || config::get().ui.show_projects_tab)
        .collect()
}

#[derive(Clone, Debug)]
pub struct PendingClose {
    pub pid: u32,
    pub display: String,
}

/// Pending project-task deletion. Shown via the same `ConfirmClose` view
/// as session close, distinguished by the [`PendingConfirm::TaskDelete`]
/// variant (vs. [`PendingConfirm::Close`]).
#[derive(Clone, Debug)]
pub struct PendingTaskDelete {
    pub project_id: String,
    pub task_id: String,
    pub display: String,
    /// tmux name of the orchestrator, captured at delete-prompt time so a
    /// concurrent state rewrite can't change what we kill.
    pub orchestrator_tmux: Option<String>,
    /// True when the delete was initiated from the Backlog popup, so the
    /// confirm/cancel return path lands back on the popup instead of the Grid.
    pub from_backlog: bool,
}

/// Pending registry-level project removal. Shown via the same
/// `ConfirmClose` view as task delete, distinguished by the
/// [`PendingConfirm::ProjectDelete`] variant.
#[derive(Clone, Debug)]
pub struct PendingProjectDelete {
    pub project_id: String,
    pub display: String,
}

/// Pending project-task orchestrator restart. Shown via the same
/// `ConfirmClose` view as destructive actions because it kills/replaces
/// runtime state even though task history is preserved.
#[derive(Clone, Debug)]
pub struct PendingTaskRestart {
    pub project_id: String,
    pub task_id: String,
    pub display: String,
}

/// The single destructive/interrupting action staged behind
/// [`View::ConfirmClose`]. Replaces four parallel `Option<…>` fields whose
/// "exactly one is `Some`" invariant used to live only in convention: with
/// one `Option<PendingConfirm>` that invariant is unrepresentable.
#[derive(Clone, Debug)]
pub enum PendingConfirm {
    Close(PendingClose),
    TaskDelete(PendingTaskDelete),
    ProjectDelete(PendingProjectDelete),
    TaskRestart(PendingTaskRestart),
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
    pub sessions: SessionsView,
    pub projects: ProjectsView,
    pub metrics: MetricsView,
    pub tasks: TasksView,
    pub todo: TodoPanelState,
    pub view: View,
    pub detail: Option<SessionDetail>,
    pub detail_loading: bool,
    pub popup_scroll: u16,
    pub should_quit: bool,
    pub last_refresh: Instant,
    pub live_view: Option<LiveView>,
    pub status_msg: Option<(String, Instant)>,
    /// The single staged destructive/interrupting action behind
    /// [`View::ConfirmClose`]. At most one can be pending at a time, which
    /// the [`PendingConfirm`] enum makes structural rather than conventional.
    pub pending_confirm: Option<PendingConfirm>,
    pub state_debug: Option<(SessionInfo, StateExplanation)>,
    pub state_debug_lines: Vec<Line<'static>>,
    pub state_debug_scroll: u16,
    pub usage: Option<UsageInfo>,
    pub usage_line: Line<'static>,
    pub session_counts: SessionCounts,
    pub prompt_buffer: String,
    /// In-progress edit buffer for the rename-session prompt (`r` on the
    /// Sessions tab). Prefilled with the current title so the user edits
    /// rather than retypes.
    pub rename_buffer: String,
    /// `session_id` being renamed while [`View::RenameSession`] is open, so
    /// the submit can target the right session even if the selection moves
    /// underneath the modal on a rescan.
    pub rename_target: Option<String>,
    pub dispatch_target: Option<(u32, String, String)>,
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
    /// Terminal-graphics picker, initialised once after entering the alt
    /// screen. `None` when running headless / `--no-tui` / inside tests so
    /// the renderer can fall back to a placeholder rather than crash.
    pub image_picker: Option<ratatui_image::picker::Picker>,
    /// Per-artifact decoded image cache, keyed by `Artifact::path`. Populated
    /// lazily on first popup render so non-image work doesn't pay decode
    /// cost; entries persist for the App lifetime since artifact paths are
    /// content-addressed and don't mutate.
    pub artifact_images: HashMap<String, ratatui_image::protocol::StatefulProtocol>,
    /// Paths whose decode failed once — never retry, since decoding the same
    /// bytes will keep failing and we'd burn CPU on every redraw.
    pub artifact_image_failed: HashSet<String>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            sessions: SessionsView::new(),
            projects: ProjectsView::new(),
            metrics: MetricsView::new(),
            tasks: TasksView::new(),
            todo: TodoPanelState::new(),
            view: View::Grid,
            detail: None,
            detail_loading: false,
            popup_scroll: 0,
            should_quit: false,
            last_refresh: Instant::now(),
            live_view: None,
            status_msg: None,
            pending_confirm: None,
            state_debug: None,
            state_debug_lines: Vec::new(),
            state_debug_scroll: 0,
            usage: None,
            usage_line: Line::default(),
            session_counts: SessionCounts::default(),
            prompt_buffer: String::new(),
            rename_buffer: String::new(),
            rename_target: None,
            dispatch_target: None,
            tmux_pane: None,
            folder_picker: None,
            bookmarks: Bookmarks::load(),
            gh_create_input: None,
            current_tab: Tab::Sessions,
            pending_dispatch: VecDeque::new(),
            last_dispatch_probe_at: None,
            image_picker: None,
            artifact_images: HashMap::new(),
            artifact_image_failed: HashSet::new(),
        }
    }

    /// Open the Sessions-tab to-do side panel. Reloads the list from disk so
    /// edits that landed since it was last open (another instance, the file
    /// hand-edited) show up, clamps the cursor in case the list shrank, and
    /// starts in navigation mode rather than add mode.
    pub fn enter_todo_panel(&mut self) {
        self.view = View::TodoPanel;
        self.todo.reload();
    }

    /// Close the panel and return to the grid, discarding any in-progress add.
    pub fn close_todo_panel(&mut self) {
        self.view = View::Grid;
        self.todo.reset_add();
    }

    /// Delete every completed item in one stroke, keeping the cursor in range.
    /// Surfaces a status line so the bulk removal is visible — unlike the
    /// single-item delete, the user can't see at a glance what just vanished.
    pub fn todo_clear_completed(&mut self) {
        let removed = self.todo.clear_completed();
        if removed > 0 {
            self.set_status(format!(
                "cleared {} completed task{}",
                removed,
                if removed == 1 { "" } else { "s" }
            ));
        } else {
            self.set_status("no completed tasks to clear".to_string());
        }
    }

    /// Open the add-task popup on the Tasks tab.
    pub fn enter_task_input(&mut self) {
        self.tasks.input.clear();
        self.tasks.renaming = None;
        self.view = View::TaskInput;
    }

    /// `r` on a focused task: reopen the task popup prefilled with the
    /// current text; Enter commits it in place (id, status and any agent
    /// binding survive). Returns false when no task is focused.
    pub fn enter_task_rename(&mut self) -> bool {
        let Some(t) = self.selected_board_task() else {
            return false;
        };
        let (id, text) = (t.id.clone(), t.text.clone());
        self.tasks.renaming = Some(id);
        self.tasks.input = text;
        self.view = View::TaskInput;
        true
    }

    pub fn close_task_input(&mut self) {
        self.tasks.input.clear();
        self.tasks.renaming = None;
        self.view = View::Grid;
    }

    /// Commit the task popup: append to To-Do and move the cursor to the
    /// new card, or — when renaming — replace the task's text in place.
    /// Returns false when the input was empty (nothing changed).
    pub fn submit_task_input(&mut self) -> bool {
        let text = std::mem::take(&mut self.tasks.input);
        self.view = View::Grid;
        if let Some(id) = self.tasks.renaming.take() {
            return self.tasks.board.rename(&id, &text);
        }
        match self.tasks.board.add(&text) {
            Some(id) => {
                self.focus_task(&id);
                true
            }
            None => false,
        }
    }

    /// `t` on a focused task: open the inline tag editor prefilled with the
    /// task's current tags (space-separated). Returns false when no task is
    /// focused.
    pub fn enter_task_tags(&mut self) -> bool {
        let Some(t) = self.selected_board_task() else {
            return false;
        };
        let (id, prefill) = (t.id.clone(), t.tags.join(" "));
        self.tasks.tagging = Some(id);
        self.tasks.input = prefill;
        self.view = View::TaskTags;
        true
    }

    pub fn close_task_tags(&mut self) {
        self.tasks.input.clear();
        self.tasks.tagging = None;
        self.view = View::Grid;
    }

    /// Commit the tag editor: parse the buffer into the normalized tag set and
    /// replace the task's tags (an empty buffer clears them). The cursor
    /// follows the card by id — tags don't reorder columns, but this keeps the
    /// same focus contract as the other task mutations. Returns false when no
    /// task was being edited.
    pub fn submit_task_tags(&mut self) -> bool {
        let text = std::mem::take(&mut self.tasks.input);
        self.view = View::Grid;
        let Some(id) = self.tasks.tagging.take() else {
            return false;
        };
        self.tasks.board.set_tags(&id, crate::tasks::parse_tags(&text));
        self.focus_task(&id);
        true
    }

    /// Tasks in `status` in display order: To-Do and Done keep the board's
    /// insertion order; the live columns (Planning and In Progress) follow
    /// the frozen needs-input float captured on tab entry
    /// ([`TasksView::in_progress_order`]), with tasks that joined since then
    /// after it in insertion order. Every column then sorts by priority (P1
    /// at the top). Selection ([`Self::selected_board_task`]) and focus
    /// ([`Self::focus_task`]) resolve against this same ordering, so cursor
    /// row N is always the Nth rendered card.
    pub fn task_column(&self, status: TaskItemStatus) -> Vec<&TaskItem> {
        let mut tasks = self.tasks.board.column(status);
        if matches!(status, TaskItemStatus::Planning | TaskItemStatus::InProgress) {
            let frozen = |id: &str| self.tasks.in_progress_order.iter().position(|x| x == id);
            // Stable sort: ids missing from the frozen order all key to MAX
            // and keep their relative insertion order at the tail.
            tasks.sort_by_key(|t| frozen(&t.id).unwrap_or(usize::MAX));
        }
        // Priority is the primary order in every column (P1 at the top). The
        // sort is stable, so equal-priority tasks keep the order established
        // above — insertion order, or the live columns' frozen needs-input
        // float.
        tasks.sort_by_key(|t| t.priority);
        tasks
    }

    /// Recompute the live columns' display order: cards whose agent is
    /// blocked on input float to the top of their column (stable within each
    /// group) so work that needs a human is seen first. Spans both Planning
    /// and In Progress. Called on Tasks-tab entry — not on scan ticks — so
    /// live state flips never reorder cards under the cursor while the user
    /// is navigating; the float settles each time the tab is (re-)opened.
    pub fn refresh_in_progress_order(&mut self) {
        let order: Vec<String> = {
            let by_tmux = self.sessions_by_tmux();
            let mut order = Vec::new();
            for status in [TaskItemStatus::Planning, TaskItemStatus::InProgress] {
                let mut tasks = self.tasks.board.column(status);
                tasks.sort_by_key(|t| {
                    !t.tmux
                        .as_deref()
                        .and_then(|n| by_tmux.get(n))
                        .is_some_and(|s| s.needs_attention())
                });
                order.extend(tasks.iter().map(|t| t.id.clone()));
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
    pub fn task_display_column(&self, col: TaskItemStatus) -> Vec<&TaskItem> {
        let statuses = column_statuses(col);
        if statuses.len() == 1 {
            return self.task_column(statuses[0]);
        }
        // Merged In Progress (absorbing Planning): both are live columns, so
        // apply the same frozen needs-input float then priority sort as
        // `task_column` does for a single live column.
        let mut tasks: Vec<&TaskItem> = statuses
            .iter()
            .flat_map(|s| self.tasks.board.column(*s))
            .collect();
        let frozen = |id: &str| self.tasks.in_progress_order.iter().position(|x| x == id);
        tasks.sort_by_key(|t| frozen(&t.id).unwrap_or(usize::MAX));
        tasks.sort_by_key(|t| t.priority);
        tasks
    }

    /// The task under the kanban cursor, resolved against the display
    /// ordering of [`Self::task_display_column`].
    pub fn selected_board_task(&self) -> Option<&TaskItem> {
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
                .position(|t| t.id == id);
            if let Some(ri) = row {
                self.tasks.col = ci;
                self.tasks.row = ri;
                return;
            }
        }
    }

    /// Space on the board, status-aware: a Planning card tells its agent to
    /// proceed with the implementation; anything else toggles Done. Returns
    /// `None` when no task is focused.
    pub fn task_space_action(&mut self) -> Option<String> {
        match self.selected_board_task()?.status {
            TaskItemStatus::Planning => Some(self.proceed_selected_task()),
            _ => self.toggle_task_done(),
        }
    }

    /// Approve the focused Planning card's plan: deliver
    /// [`PROCEED_PROMPT`] to the bound agent and move the card to In
    /// Progress. A live session gets the prompt immediately (if the agent is
    /// still planning, it lands as the queued next message); a dead tmux
    /// with a known session id is respawned with resume and the prompt
    /// queued for dispatch once it reports Idle. With nothing to deliver to,
    /// the card stays in Planning so the column never lies about an agent
    /// actually implementing.
    pub fn proceed_selected_task(&mut self) -> String {
        let Some(t) = self.selected_board_task() else {
            return "no task focused".into();
        };
        let id = t.id.clone();
        let preview = crate::models::first_line_truncated(&t.text, 32);
        let live_tmux = t
            .tmux
            .clone()
            .filter(|n| crate::send::tmux_session_exists(n));
        if let Some(tmux) = live_tmux {
            return match crate::send::send_prompt(&tmux, PROCEED_PROMPT) {
                Ok(()) => {
                    self.tasks.board.set_status(&id, TaskItemStatus::InProgress);
                    self.focus_task(&id);
                    format!("proceeding: {} — agent told to implement [{}]", preview, tmux)
                }
                Err(e) => format!("proceed failed: {} — task stays in planning", e),
            };
        }
        let (sid, cwd, agent_id) = {
            let Some(t) = self.tasks.board.get(&id) else {
                return "task vanished".into();
            };
            match (t.session_id.clone(), t.cwd.clone()) {
                (Some(sid), Some(cwd)) => (
                    sid,
                    cwd,
                    t.agent_id.clone().unwrap_or_else(|| "claude".into()),
                ),
                _ => {
                    return "agent session is gone and its session id was never seen — press s to re-assign"
                        .into();
                }
            }
        };
        match crate::spawn::spawn_agent_session(
            &agent_id,
            &cwd,
            Some(crate::spawn::ResumeTarget::SessionId(sid)),
            None,
            false,
        ) {
            Ok(tmux) => {
                self.queue_pending_dispatch(tmux.clone(), PROCEED_PROMPT.to_string());
                self.tasks.board.rebind_tmux(&id, &tmux);
                self.tasks.board.set_status(&id, TaskItemStatus::InProgress);
                self.focus_task(&id);
                format!("proceeding: {} — agent resumed [{}]", preview, tmux)
            }
            Err(e) => format!("proceed failed: resume error: {}", e),
        }
    }

    /// Flip the focused task between Done and To-Do (an In Progress task
    /// goes to Done — finishing an agent task by hand is always allowed).
    /// Completing a task closes its live agent session; the binding
    /// (`tmux`/`session_id`) is kept so `f` on the Done card can still
    /// resume the transcript. Returns a status line describing the move,
    /// or `None` when no task is focused.
    pub fn toggle_task_done(&mut self) -> Option<String> {
        let t = self.selected_board_task()?;
        let id = t.id.clone();
        let preview = crate::models::first_line_truncated(&t.text, 32);
        let tmux = t.tmux.clone();
        let to = match t.status {
            TaskItemStatus::Done => TaskItemStatus::Todo,
            _ => TaskItemStatus::Done,
        };
        self.tasks.board.set_status(&id, to);
        self.tasks.clamp_row();
        if to != TaskItemStatus::Done {
            return Some(format!("reopened: {}", preview));
        }
        let live = tmux
            .as_deref()
            .filter(|n| crate::send::tmux_session_exists(n));
        Some(match live {
            Some(name) => match crate::send::kill_tmux_session(name) {
                Ok(()) => format!("done: {} — closed agent session [{}]", preview, name),
                Err(e) => format!(
                    "done: {} — closing agent session [{}] failed: {}",
                    preview, name, e
                ),
            },
            None => format!("done: {}", preview),
        })
    }

    /// `1`–`4` on a focused task: set its priority. Priority is the column's
    /// primary sort key, so the card may jump to a new row; the cursor rides
    /// with it (resolved by id through [`Self::focus_task`]). Returns a status
    /// line, or `None` when no task is focused.
    pub fn set_selected_task_priority(&mut self, priority: TaskPriority) -> Option<String> {
        let t = self.selected_board_task()?;
        let id = t.id.clone();
        let preview = crate::models::first_line_truncated(&t.text, 32);
        self.tasks.board.set_priority(&id, priority);
        self.focus_task(&id);
        Some(format!("{} · {}", priority.label(), preview))
    }

    /// Candidates for the task-assign places picker: registered projects
    /// first, then bookmarks, then recently-used directories (other board
    /// tasks, scanned sessions) newest-first — deduped by path with the
    /// labelled project entry winning. This order is what an empty filter
    /// shows.
    pub fn known_places(&self) -> Vec<Place> {
        use crate::folder_picker::PlaceSource;
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut out: Vec<Place> = Vec::new();
        for p in &self.projects.snapshot.projects {
            if seen.insert(p.root.clone()) {
                out.push(Place::new(
                    Some(p.name.clone()),
                    p.root.clone(),
                    PlaceSource::Project,
                ));
            }
        }
        for path in self.bookmarks.list() {
            if seen.insert(path.clone()) {
                out.push(Place::new(None, path, PlaceSource::Bookmark));
            }
        }
        // Recents carry a coarse timestamp purely for ordering: a task's
        // creation (or completion) time, a session's last activity.
        let mut recents: Vec<(u64, PathBuf)> = Vec::new();
        for t in self.tasks.board.tasks() {
            if let Some(cwd) = t.cwd.as_deref() {
                recents.push((t.created_at.max(t.done_at.unwrap_or(0)), PathBuf::from(cwd)));
            }
        }
        for s in &self.sessions.last_sessions {
            recents.push((
                s.last_activity.unwrap_or(s.started_at),
                PathBuf::from(&s.cwd),
            ));
        }
        recents.sort_by_key(|(ts, _)| std::cmp::Reverse(*ts));
        const MAX_RECENTS: usize = 15;
        let mut added = 0usize;
        for (_, path) in recents {
            if added >= MAX_RECENTS {
                break;
            }
            if seen.insert(path.clone()) {
                out.push(Place::new(None, path, PlaceSource::Recent));
                added += 1;
            }
        }
        out
    }

    /// Places for the assign picker: [`Self::known_places`] with the most
    /// recently assigned cwd promoted to the front (and thus selected by
    /// default), so firing several tasks at one project is a plain Enter
    /// each time. A last cwd no longer among the known places is
    /// resurrected as a Recent entry.
    fn assign_places(&self) -> Vec<Place> {
        use crate::folder_picker::PlaceSource;
        let mut places = self.known_places();
        if let Some(last) = self.tasks.board.last_assign_cwd() {
            let last = std::path::Path::new(last);
            // `S` quick-assigns at $HOME; that's not a project choice
            // worth promoting (or resurrecting) here.
            if Some(last) == dirs::home_dir().as_deref() {
                return places;
            }
            if let Some(idx) = places.iter().position(|p| p.path == last) {
                let place = places.remove(idx);
                places.insert(0, place);
            } else {
                places.insert(0, Place::new(None, last.to_path_buf(), PlaceSource::Recent));
            }
        }
        places
    }

    /// `s` on a focused task: open the places picker (registered projects,
    /// bookmarks, recent dirs — fuzzy-filterable) to choose the cwd the
    /// agent will run in, falling back to the filesystem browser when
    /// nothing is known yet. Returns false when no task is focused or the
    /// task is already Done.
    pub fn enter_task_assign_picker(&mut self) -> bool {
        let Some(t) = self.selected_board_task() else {
            return false;
        };
        if t.status == TaskItemStatus::Done {
            return false;
        }
        let id = t.id.clone();
        let prev_cwd = t.cwd.clone();
        let places = self.assign_places();
        self.tasks.pending_assign = Some(id);
        if places.is_empty() {
            self.folder_picker = Some(FolderPicker::new(Self::assign_browse_start(
                prev_cwd.as_deref(),
            )));
        } else {
            let mut picker = FolderPicker::new_places(places);
            if let Some(cwd) = prev_cwd.as_deref() {
                picker.select_path(std::path::Path::new(cwd));
            }
            self.folder_picker = Some(picker);
        }
        self.view = View::FolderPicker;
        true
    }

    /// `S` on a focused task: skip the picker and spawn the agent right
    /// away with `$HOME` as the cwd — for broad questions not tied to any
    /// project yet; where to go next gets figured out with the agent.
    /// Returns None when no task is focused or the task is already Done.
    pub fn assign_selected_task_at_home(&mut self) -> Option<String> {
        let t = self.selected_board_task()?;
        if t.status == TaskItemStatus::Done {
            return None;
        }
        let id = t.id.clone();
        self.tasks.pending_assign = Some(id);
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        Some(self.assign_task_agent(&home.display().to_string()))
    }

    /// Tab in the places/browse picker: flip between the known-places list
    /// and the filesystem browser. Serves both the task-assign flow and the
    /// Sessions-tab `N` flow; no-op in the Projects flows (register/new
    /// task) and when there is nothing to flip to.
    pub fn toggle_places_picker_mode(&mut self) {
        if self.projects.creating_task || self.projects.registering_only {
            return;
        }
        let Some(picker) = self.folder_picker.as_ref() else {
            return;
        };
        let assigning = self.tasks.pending_assign.clone();
        match picker.mode {
            PickerMode::Places => {
                let prev_cwd = match &assigning {
                    Some(id) => self.tasks.board.get(id).and_then(|t| t.cwd.clone()),
                    None => self.selected_session_info().map(|s| s.cwd.clone()),
                };
                self.folder_picker = Some(FolderPicker::new(Self::assign_browse_start(
                    prev_cwd.as_deref(),
                )));
            }
            PickerMode::Browse => {
                let places = if assigning.is_some() {
                    self.assign_places()
                } else {
                    self.known_places()
                };
                if !places.is_empty() {
                    self.folder_picker = Some(FolderPicker::new_places(places));
                }
            }
            PickerMode::Bookmarks => {}
        }
    }

    /// Browse-mode starting point for an assignment: the task's previous
    /// cwd, else `$HOME`, else `/`.
    fn assign_browse_start(prev_cwd: Option<&str>) -> PathBuf {
        prev_cwd
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    /// Picker chose `cwd` for the pending assignment: spawn the default
    /// agent there, hand it the task text wrapped in plan-first framing
    /// ([`planning_prompt`] — inline when the agent supports a spawn-time
    /// prompt, otherwise queued for dispatch once the session reports Idle —
    /// Claude ignores spawn-time prompts), record the binding, and move the
    /// card to Planning. Space on the card later approves the plan and
    /// promotes it to In Progress. Returns the status line.
    pub fn assign_task_agent(&mut self, cwd: &str) -> String {
        let pending = self.tasks.pending_assign.take();
        self.close_folder_picker();
        let Some(id) = pending else {
            return "no task pending assignment".into();
        };
        let Some(task) = self.tasks.board.get(&id) else {
            return "task vanished before assignment".into();
        };
        let prompt = planning_prompt(&task.text);
        let agent_id = config::get().default_session_agent_id();
        let supports_initial_prompt = config::get()
            .agent(&agent_id)
            .is_some_and(|a| a.supports_initial_prompt());
        match crate::spawn::spawn_agent_session(
            &agent_id,
            cwd,
            None,
            supports_initial_prompt.then_some(prompt.as_str()),
            false,
        ) {
            Ok(tmux) => {
                if !supports_initial_prompt {
                    self.queue_pending_dispatch(tmux.clone(), prompt);
                }
                self.tasks.board.assign(&id, cwd, &agent_id, &tmux);
                self.focus_task(&id);
                format!("assigned {} [{}] — planning (Space approves the plan)", agent_id, tmux)
            }
            Err(e) => format!("assign failed: {}", e),
        }
    }

    /// Point a task's binding at a freshly-spawned mux session (resume path).
    pub fn rebind_task_tmux(&mut self, id: &str, tmux: &str) {
        self.tasks.board.rebind_tmux(id, tmux);
    }

    /// Delete the focused task. The bound agent session (if any) is left
    /// running — it still shows on the Sessions tab. Returns the status line.
    pub fn delete_selected_task(&mut self) -> Option<String> {
        let id = self.selected_board_task()?.id.clone();
        let removed = self.tasks.board.remove(&id)?;
        self.tasks.clamp_row();
        let preview = crate::models::first_line_truncated(&removed.text, 32);
        Some(match removed.tmux {
            Some(tmux) => format!(
                "deleted: {} — agent session [{}] left running (close it from Sessions)",
                preview, tmux
            ),
            None => format!("deleted: {}", preview),
        })
    }

    pub fn clear_done_tasks(&mut self) {
        let removed = self.tasks.board.clear_done();
        self.tasks.clamp_row();
        if removed > 0 {
            self.set_status(format!(
                "cleared {} done task{}",
                removed,
                if removed == 1 { "" } else { "s" }
            ));
        } else {
            self.set_status("no done tasks to clear".to_string());
        }
    }

    pub fn toggle_show_inactive(&mut self) {
        self.sessions.show_inactive = !self.sessions.show_inactive;
        self.rebuild_groups();
    }

    pub fn toggle_show_orch_workers(&mut self) {
        self.sessions.show_orch_workers = !self.sessions.show_orch_workers;
        self.rebuild_groups();
    }

    pub fn set_tab(&mut self, tab: Tab) {
        // Entering the Tasks tab re-reads the board so edits from another
        // instance (or a hand-edited tasks.json) show up, mirroring the
        // to-do panel's reload-on-open. The reload and the re-floated
        // In Progress order can both rearrange rows, so the cursor is
        // re-anchored to the task it was on (by id) rather than left at a
        // stale (col, row) pointing at whatever card landed there.
        if tab == Tab::Tasks && self.current_tab != Tab::Tasks {
            let keep = self.selected_board_task().map(|t| t.id.clone());
            self.tasks.reload();
            self.refresh_in_progress_order();
            if let Some(id) = keep {
                self.focus_task(&id);
            }
            // No-op after a successful re-focus; catches the id having been
            // deleted out from under us (and the no-selection case).
            self.tasks.clamp_row();
        }
        self.current_tab = tab;
    }

    pub fn cycle_tab(&mut self) {
        let tabs = visible_tabs();
        let next = match tabs.iter().position(|t| *t == self.current_tab) {
            Some(i) => tabs[(i + 1) % tabs.len()],
            // Current tab got hidden out from under us (config reload via
            // restart can't do this mid-run, but stay defensive): land on
            // the first visible tab rather than panicking.
            None => tabs.first().copied().unwrap_or(Tab::Sessions),
        };
        self.set_tab(next);
    }

    pub fn cycle_tab_back(&mut self) {
        let tabs = visible_tabs();
        let prev = match tabs.iter().position(|t| *t == self.current_tab) {
            Some(i) => tabs[(i + tabs.len() - 1) % tabs.len()],
            None => tabs.first().copied().unwrap_or(Tab::Sessions),
        };
        self.set_tab(prev);
    }

    /// Apply a fresh projects snapshot. Returns true when the snapshot
    /// differs from the current one — unchanged ticks skip the cursor
    /// bookkeeping (and the caller skips the repaint), mirroring
    /// [`Self::update_sessions`].
    pub fn update_projects(&mut self, snap: ProjectsSnapshot) -> bool {
        // A pending focus must keep polling even on identical snapshots: its
        // budget counts scan ticks, and the task it waits for may only gain
        // a tmux session (not a snapshot change) when the orchestrator boots.
        if self.projects.pending_focus_task_id.is_none() && snap == self.projects.snapshot {
            return false;
        }
        let pv = &mut self.projects;
        // Preserve cursor when possible: keep the same project_id selected
        // across rescans even if the order shifted.
        let prev_pid = pv.snapshot.projects.get(pv.sel).map(|p| p.id.clone());
        // Track the focused task by id so a status transition (Running →
        // Review etc.) carries the cursor across columns. Mirrors the
        // prev_sid trick in `update_metrics`.
        let prev_task_id = pv.selected_project_task().map(|t| t.task_id.clone());
        let first_load = pv.snapshot.projects.is_empty();
        pv.snapshot = snap;
        if let Some(pid) = prev_pid {
            if let Some(idx) = pv.snapshot.projects.iter().position(|p| p.id == pid) {
                pv.sel = idx;
            }
        }
        // If the task is gone, fall through and let clamp handle the row.
        if let Some(task_id) = prev_task_id {
            pv.focus_task(&task_id);
        }
        // Jump-if-empty only on the very first load — once the user is in the
        // tab, an empty focused column means they explicitly navigated there
        // (or a task drained out of it), and silently overriding their
        // selection on every rescan is the bug we're avoiding.
        if first_load {
            pv.clamp_cursor_jump_if_empty();
        } else {
            pv.clamp_cursor();
        }
        if let Some(task_id) = self.projects.pending_focus_task_id.clone() {
            if let Some(col) = self.projects.focus_task(&task_id) {
                self.set_status(format!(
                    "started {} — focus moved to {}",
                    crate::orchestrator::short_task_id(&task_id),
                    kanban_col_name(col),
                ));
                self.projects.pending_focus_task_id = None;
                self.projects.pending_focus_budget = 0;
            } else {
                self.projects.pending_focus_budget =
                    self.projects.pending_focus_budget.saturating_sub(1);
                if self.projects.pending_focus_budget == 0 {
                    self.set_status(format!(
                        "started {} — orchestrator booting; cursor unchanged",
                        crate::orchestrator::short_task_id(&task_id),
                    ));
                    self.projects.pending_focus_task_id = None;
                }
            }
        }
        // Newly-discovered orchestrator/worker tmux names need to disappear
        // from the Sessions view immediately; without this the hide flag
        // would only take effect on the next session scan.
        self.rebuild_groups();
        true
    }

    pub fn selected_project(&self) -> Option<&crate::orchestrator::Project> {
        self.projects.selected_project()
    }

    pub fn kanban_column_tasks(&self, col: usize) -> Vec<&crate::orchestrator::TaskState> {
        self.projects.kanban_column_tasks(col)
    }

    pub fn backlog_tasks(&self) -> Vec<&crate::orchestrator::TaskState> {
        self.projects.backlog_tasks()
    }

    pub fn open_backlog(&mut self) {
        self.projects.backlog_sel = 0;
        self.view = View::Backlog;
    }

    pub fn close_backlog(&mut self) {
        self.view = View::Grid;
    }

    pub fn selected_backlog_task(&self) -> Option<&crate::orchestrator::TaskState> {
        self.projects.selected_backlog_task()
    }

    pub fn selected_project_task(&self) -> Option<&crate::orchestrator::TaskState> {
        self.projects.selected_project_task()
    }

    /// Approve the focused Review task. If the task has a PR, flip
    /// `pr.review_state` to `Approved`, snapshot the branch/base SHAs so
    /// `pr merge` can detect whether main moved between approval and
    /// merge, and transition the task to `Merging` so the card moves to
    /// the Merging column. If another task in the same project currently
    /// holds the merge lock, the task still moves — the renderer paints
    /// a queued border in muted gray so the user sees approval landed
    /// even though the actual merge waits its turn. Tmux sessions stay
    /// alive; they're torn down by `pr finalize` after the merge lands.
    /// If the task has no PR (a research/queueing task delivered via
    /// `task report --status done`, auto-routed into Review), transition
    /// it directly to `Done` and tear down the orchestrator tmux. The
    /// returned [`ApproveOutcome`] tells the caller whether to show the
    /// generic "nothing to approve" toast and whether to ping the live
    /// orchestrator tmux.
    pub fn approve_review_task(&mut self) -> ApproveOutcome {
        use crate::orchestrator::TaskStatus;
        let Some(t) = self.selected_project_task() else {
            return ApproveOutcome::NotReviewTask;
        };
        if t.status != TaskStatus::Review {
            return ApproveOutcome::NotReviewTask;
        }
        let project_id = t.project_id.clone();
        let task_id = t.task_id.clone();

        // The read distinguishes the PR-less Done path from a real PR;
        // for the latter, the approval itself (SHA snapshot + review_state
        // flip) is delegated to the shared `ops::pr` implementation so the
        // TUI and CLI record identical state.
        let pr = match crate::pr::read_pr(&project_id, &task_id) {
            Ok(Some(_)) => match crate::ops::pr::pr_approve(&project_id, &task_id) {
                Ok(pr) => pr,
                Err(e) => {
                    self.set_status(format!("approve failed: {}", e));
                    return ApproveOutcome::Failed;
                }
            },
            Ok(None) => {
                match crate::orchestrator::update_task_state(&project_id, &task_id, |s| {
                    s.status = TaskStatus::Done;
                }) {
                    Ok(state) => {
                        crate::orchestrator::cleanup_task_sessions(&state);
                        self.set_status(format!(
                            "approved PR-less task {} → Done",
                            crate::orchestrator::short_task_id(&task_id)
                        ));
                        return ApproveOutcome::DoneNoPr;
                    }
                    Err(e) => {
                        self.set_status(format!("approve failed: {}", e));
                        return ApproveOutcome::Failed;
                    }
                }
            }
            Err(e) => {
                self.set_status(format!("approve: pr read failed: {}", e));
                return ApproveOutcome::Failed;
            }
        };
        let pr_id = pr.id;
        let lock_holder = crate::merge_lock::current_holder(&project_id)
            .ok()
            .flatten();
        let queued_behind = lock_holder
            .as_ref()
            .filter(|h| h.task_id != task_id)
            .map(|h| h.task_id.clone());
        if let Err(e) = crate::orchestrator::update_task_state(&project_id, &task_id, |s| {
            s.status = TaskStatus::Merging;
            s.note = Some(match &queued_behind {
                Some(other) => format!(
                    "PR #{}: approved; queued behind {}",
                    pr_id,
                    crate::orchestrator::short_task_id(other),
                ),
                None => format!("PR #{}: approved; merging", pr_id),
            });
        }) {
            self.set_status(format!("approve: state update failed: {}", e));
            return ApproveOutcome::Failed;
        }

        // Cursor stays on the same task; it's now in the Merging column.
        // The caller is responsible for notifying the live orchestrator
        // tmux to continue the merge flow.
        self.set_status(match &queued_behind {
            Some(other) => format!(
                "approved PR #{} for {} — queued behind {}",
                pr.id,
                crate::orchestrator::short_task_id(&task_id),
                crate::orchestrator::short_task_id(other),
            ),
            None => format!(
                "approved PR #{} for {}",
                pr.id,
                crate::orchestrator::short_task_id(&task_id),
            ),
        });
        ApproveOutcome::PrApproved
    }

    /// Undo the `Review → Merging` transition written by
    /// [`Self::approve_review_task`] when the post-approve notify discovers
    /// there's no live orchestrator tmux to drive the merge. Without this the
    /// card would strand in the Merging column with a dead orchestrator and no
    /// way to act on it. Flipping it back to Review keeps the card actionable:
    /// the user can re-approve (re-ping) or resurrect (`f`). The PR's
    /// `review_state` stays `Approved` — only the task status rolls back —
    /// because the approval itself is still valid; we're just no longer
    /// claiming the merge is underway. Best-effort: a failed write leaves the
    /// card in Merging, which is no worse than before.
    pub fn rollback_merging_to_review(&mut self, project_id: &str, task_id: &str) {
        use crate::orchestrator::TaskStatus;
        let _ = crate::orchestrator::update_task_state(project_id, task_id, |s| {
            if s.status == TaskStatus::Merging {
                s.status = TaskStatus::Review;
                s.note = Some("approved; orchestrator not live — re-approve or resurrect".into());
            }
        });
    }

    /// `tmux_session_name → SessionInfo` over the latest scan. Built fresh
    /// per call so it always reflects [`Self::last_sessions`]. Used by the
    /// Projects view to enrich task cards with live agent state (context
    /// tokens, current tool, idle/processing/waiting).
    pub fn sessions_by_tmux(&self) -> HashMap<&str, &SessionInfo> {
        let mut out = HashMap::new();
        for s in &self.sessions.last_sessions {
            if let Some(name) = s.tmux_session.as_deref() {
                out.insert(name, s);
            }
        }
        out
    }

    pub fn update_metrics(&mut self, m: MetricsAnalysis) {
        self.metrics.update(m);
    }

    pub fn update_metrics_progress(&mut self, scanned: usize, total: usize) {
        self.metrics.update_progress(scanned, total);
    }

    pub fn selected_metrics_session(&self) -> Option<&SelectableSession> {
        self.metrics.selected_session()
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

    /// `N` on the Sessions tab: the same places picker the task-assign
    /// flow uses (registered projects, bookmarks, recent dirs —
    /// fuzzy-filterable) to choose where the new session spawns, falling
    /// back to the filesystem browser when nothing is known yet. The
    /// selected session's cwd starts highlighted.
    pub fn enter_session_places_picker(&mut self) {
        let places = self.known_places();
        if places.is_empty() {
            self.enter_folder_picker();
            return;
        }
        let mut picker = FolderPicker::new_places(places);
        if let Some(cwd) = self.selected_session_info().map(|s| s.cwd.clone()) {
            picker.select_path(std::path::Path::new(&cwd));
        }
        self.folder_picker = Some(picker);
        self.view = View::FolderPicker;
    }

    /// Open the picker pre-loaded with the user's bookmarked folders.
    /// Returns `false` (no-op) when no bookmarks exist so the caller can
    /// show a hint instead of silently opening an empty popup.
    pub fn enter_bookmarks_picker(&mut self) -> bool {
        let entries = self.bookmarks.list();
        if entries.is_empty() {
            return false;
        }
        self.folder_picker = Some(FolderPicker::new_bookmarks(entries));
        self.view = View::FolderPicker;
        true
    }

    /// Toggle the bookmark on the highlighted picker entry. Returns the
    /// new state plus a display path the caller can use for status text,
    /// or `None` when no entry is selected. Also keeps the picker view
    /// in sync: a toggle-off in Bookmarks mode removes the row so the
    /// list doesn't show stale entries.
    pub fn toggle_selected_bookmark(&mut self) -> Option<(bool, String)> {
        let picker = self.folder_picker.as_mut()?;
        let path = picker.selected_path()?;
        let display = path.display().to_string();
        let added = self.bookmarks.toggle(path);
        if !added && picker.mode == PickerMode::Bookmarks {
            picker.remove_selected();
        }
        Some((added, display))
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
        self.projects.creating_task = true;
        self.view = View::FolderPicker;
    }

    /// Open the folder picker in "register a project, no task" mode. The
    /// space/. picks register the chosen folder via
    /// [`Self::register_picked_project`] and exit the picker — no
    /// orchestrator is spawned.
    pub fn enter_folder_picker_for_register_only(&mut self) {
        let start = self
            .selected_project()
            .map(|p| p.root.clone())
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        self.folder_picker = Some(FolderPicker::new(start));
        self.projects.registering_only = true;
        self.view = View::FolderPicker;
    }

    /// Register `cwd` as a project (no task spawned) and close the picker.
    /// Returns the registered project name on success so callers can
    /// surface it in a status message.
    pub fn register_picked_project(&mut self, cwd: &str) -> Result<String, String> {
        let path = PathBuf::from(cwd);
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| cwd.to_string());
        let result = crate::orchestrator::ensure_project_registered(&path, &name)
            .map(|_| name)
            .map_err(|e| e.to_string());
        self.close_folder_picker();
        result
    }

    /// Picker chose `cwd` while in projects-creation mode. Stash the cwd
    /// and switch to a multi-line prompt input; the actual orchestrator
    /// spawn happens in [`Self::submit_project_task`].
    pub fn enter_project_task_prompt(&mut self, cwd: String) {
        self.folder_picker = None;
        self.projects.pending_cwd = Some(cwd);
        self.projects.pending_agent_id = None;
        self.prompt_buffer.clear();
        self.dispatch_target = None;
        self.view = View::PromptInput;
    }

    /// Cycle the orchestrator agent for the pending project task to the
    /// next entry in the resolved-agents map. No-op when fewer than two
    /// agents are configured. Called from the prompt-input Tab handler.
    pub fn cycle_pending_agent_id(&mut self) {
        if self.projects.pending_cwd.is_none() {
            return;
        }
        let agents = config::get().resolved_agents();
        let ids: Vec<String> = agents.into_keys().collect();
        if ids.len() < 2 {
            return;
        }
        let current = self
            .projects
            .pending_agent_id
            .clone()
            .unwrap_or_else(|| config::get().default_orchestrator_agent_id());
        let idx = ids.iter().position(|id| id == &current).unwrap_or(0);
        let next = ids[(idx + 1) % ids.len()].clone();
        self.projects.pending_agent_id = Some(next);
    }

    /// Display label for the agent that will run the pending project task,
    /// resolving `None` to the configured default. Returns `None` outside
    /// the project-creation flow.
    pub fn pending_agent_label(&self) -> Option<String> {
        self.projects.pending_cwd.as_ref()?;
        Some(
            self.projects
                .pending_agent_id
                .clone()
                .unwrap_or_else(|| config::get().default_orchestrator_agent_id()),
        )
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
        self.projects.creating_task = false;
        self.projects.registering_only = false;
        self.tasks.pending_assign = None;
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
        self.dispatch_target = Self::compute_dispatch_target(&self.sessions.groups);
        self.view = View::PromptInput;
    }

    pub fn close_prompt_input(&mut self) {
        self.prompt_buffer.clear();
        self.dispatch_target = None;
        self.projects.pending_cwd = None;
        self.projects.pending_agent_id = None;
        self.projects.creating_task = false;
        self.view = View::Grid;
    }

    /// True when the prompt input should be routed through the orchestrator
    /// project-task flow instead of the regular session-dispatch flow.
    pub fn prompt_input_for_project(&self) -> bool {
        self.projects.pending_cwd.is_some()
    }

    /// Consumes the pending cwd, prompt, and agent override, clears
    /// prompt input, returns the `(cwd, prompt, agent_id_override)` tuple.
    /// Returns `None` if no project task is pending. The override is
    /// `None` when the user didn't cycle off the configured default.
    pub fn submit_project_task(&mut self) -> Option<(String, String, Option<String>)> {
        let cwd = self.projects.pending_cwd.take()?;
        let agent_id = self.projects.pending_agent_id.take();
        self.projects.creating_task = false;
        self.view = View::Grid;
        let prompt = std::mem::take(&mut self.prompt_buffer);
        Some((cwd, prompt, agent_id))
    }

    pub fn submit_prompt_input(&mut self) -> String {
        self.view = View::Grid;
        std::mem::take(&mut self.prompt_buffer)
    }

    /// Open the rename-title modal for the currently selected session,
    /// prefilling the edit buffer with its existing title. Returns `false`
    /// (and changes nothing) when no session is selected.
    pub fn enter_rename_session(&mut self) -> bool {
        let Some(session) = self.selected_session_info() else {
            return false;
        };
        let sid = session.session_id.clone();
        let title = session.title.clone().unwrap_or_default();
        self.rename_target = Some(sid);
        self.rename_buffer = title;
        self.view = View::RenameSession;
        true
    }

    pub fn close_rename_session(&mut self) {
        self.rename_buffer.clear();
        self.rename_target = None;
        self.view = View::Grid;
    }

    /// Title currently being edited, for the modal's "renaming X" header.
    pub fn rename_original_title(&self) -> Option<&str> {
        let sid = self.rename_target.as_deref()?;
        self.sessions
            .groups
            .iter()
            .flat_map(|g| g.sessions.iter())
            .find(|s| s.session_id == sid)
            .and_then(|s| s.title.as_deref())
    }

    /// Commit the rename: apply the trimmed buffer to the in-memory session
    /// for instant feedback and return `(session_id, title)` for the caller
    /// to persist to the title cache. Returns `None` when the title is empty
    /// (treated as cancel) or no rename is in flight; either way the modal
    /// closes.
    pub fn submit_session_rename(&mut self) -> Option<(String, String)> {
        self.view = View::Grid;
        let sid = self.rename_target.take()?;
        let title = std::mem::take(&mut self.rename_buffer).trim().to_string();
        if title.is_empty() {
            return None;
        }
        for group in &mut self.sessions.groups {
            for session in &mut group.sessions {
                if session.session_id == sid {
                    session.title = Some(title.clone());
                    session.titling = false;
                }
            }
        }
        Some((sid, title))
    }

    pub fn dispatch_target(&self) -> Option<&(u32, String, String)> {
        self.dispatch_target.as_ref()
    }

    pub fn queue_pending_dispatch(&mut self, tmux: String, prompt: String) {
        self.pending_dispatch.push_back(PendingDispatch {
            tmux,
            prompt,
            queued_at: Instant::now(),
        });
    }

    pub fn has_pending_dispatch(&self) -> bool {
        !self.pending_dispatch.is_empty()
    }

    pub fn pending_dispatch_count(&self) -> usize {
        self.pending_dispatch.len()
    }

    /// If a pending dispatch exists and the target session now reports Idle,
    /// consume it and return [`DispatchAction::Send`]. If the deadline has
    /// passed, return [`DispatchAction::Timeout`]. Otherwise, put it back and
    /// wait. Dispatches are FIFO so multiple Claude launches can't overwrite
    /// each other's initial prompts.
    pub fn poll_pending_dispatch(&mut self) -> DispatchAction {
        let Some(pd) = self.pending_dispatch.pop_front() else {
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
        // Walk the unfiltered scan set, not `self.sessions.groups`:
        // orchestrator and worker tmux names are hidden from `groups` by the
        // Sessions view filter, so checking `groups` here would block dispatch
        // forever for the very sessions we need to dispatch into.
        let scanner_idle = self.sessions.last_sessions.iter().any(|s| {
            s.tmux_session.as_deref() == Some(pd.tmux.as_str()) && s.state == SessionState::Idle
        });
        if scanner_idle {
            let aged_in = pd.queued_at.elapsed() >= Duration::from_secs(5);
            // The `pane_ready_for_input` probe is a `tmux capture-pane`
            // fork+exec — too costly to run every ~50ms render frame. Throttle
            // it to ~2x/sec; between probes treat the pane as not-yet-ready and
            // keep waiting. The cold-boot fallback (`aged_in`) and the timeout
            // below don't need the probe, so they still fire on schedule.
            let probe_due = self
                .last_dispatch_probe_at
                .is_none_or(|t| t.elapsed() >= Duration::from_millis(500));
            let pane_ready = if aged_in {
                // aged_in already sends; don't burn a probe.
                false
            } else if probe_due {
                self.last_dispatch_probe_at = Some(Instant::now());
                crate::send::pane_ready_for_input(&pd.tmux)
            } else {
                false
            };
            if pane_ready || aged_in {
                if !pane_ready {
                    log::info!(
                        "dispatch: pane_ready=false but {}s elapsed — sending anyway (target=[{}])",
                        pd.queued_at.elapsed().as_secs(),
                        pd.tmux
                    );
                }
                self.last_dispatch_probe_at = None;
                return DispatchAction::Send {
                    tmux: pd.tmux,
                    prompt: pd.prompt,
                };
            }
        }
        if pd.queued_at.elapsed() > config::get().ui.pending_dispatch_timeout() {
            self.last_dispatch_probe_at = None;
            return DispatchAction::Timeout { tmux: pd.tmux };
        }
        self.pending_dispatch.push_front(pd);
        DispatchAction::Wait
    }

    /// Time the current pending dispatch has been waiting. None when no
    /// dispatch is queued. Used by the status bar so the user can tell
    /// at a glance that an orchestrator/dispatch is still booting rather
    /// than wondering why nothing is happening.
    pub fn pending_dispatch_age(&self) -> Option<Duration> {
        self.pending_dispatch
            .front()
            .map(|pd| pd.queued_at.elapsed())
    }

    /// Tmux session name of the current pending dispatch, if any.
    pub fn pending_dispatch_target(&self) -> Option<&str> {
        self.pending_dispatch.front().map(|pd| pd.tmux.as_str())
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

    pub fn update_session_counts(&mut self, counts: SessionCounts) {
        self.session_counts = counts;
    }

    pub fn enter_confirm_close(&mut self) {
        let Some(session) = self.selected_session_info() else {
            return;
        };
        self.pending_confirm = Some(PendingConfirm::Close(PendingClose {
            pid: session.pid,
            display: format!("{} (PID {})", session.project_name, session.pid),
        }));
        self.view = View::ConfirmClose;
    }

    pub fn cancel_confirm_close(&mut self) {
        let from_backlog = matches!(
            &self.pending_confirm,
            Some(PendingConfirm::TaskDelete(p)) if p.from_backlog
        );
        self.pending_confirm = None;
        self.view = if from_backlog {
            View::Backlog
        } else {
            View::Grid
        };
    }

    pub fn take_pending_close(&mut self) -> Option<PendingClose> {
        self.view = View::Grid;
        if matches!(self.pending_confirm, Some(PendingConfirm::Close(_))) {
            if let Some(PendingConfirm::Close(p)) = self.pending_confirm.take() {
                return Some(p);
            }
        }
        None
    }

    /// Stage a project-task deletion behind the same ConfirmClose flow used
    /// for sessions. Resolves `orchestrator_tmux` synchronously so the kill
    /// step doesn't have to re-load state.json.
    pub fn enter_confirm_task_delete(&mut self) {
        let Some(p) = self.selected_project().cloned() else {
            self.set_status("no project selected".into());
            return;
        };
        let Some(task) = self.selected_project_task().cloned() else {
            self.set_status("no task selected — focus a task on the kanban first".into());
            return;
        };
        let status_label = task.status.as_str();
        let display = format!(
            "{} — {} (task {})",
            p.name,
            status_label,
            crate::orchestrator::short_task_id(&task.task_id),
        );
        self.pending_confirm = Some(PendingConfirm::TaskDelete(PendingTaskDelete {
            project_id: p.id.clone(),
            task_id: task.task_id.clone(),
            display,
            orchestrator_tmux: task.orchestrator_tmux.clone(),
            from_backlog: false,
        }));
        self.view = View::ConfirmClose;
    }

    /// Stage a backlog-task deletion, mirroring [`Self::enter_confirm_task_delete`]
    /// but resolving the task from the Backlog popup cursor so the confirm/cancel
    /// flow returns to the popup.
    pub fn enter_confirm_backlog_task_delete(&mut self) {
        let Some(p) = self.selected_project().cloned() else {
            self.set_status("no project selected".into());
            return;
        };
        let Some(task) = self.selected_backlog_task().cloned() else {
            self.set_status("no backlog task selected".into());
            return;
        };
        if task.status != crate::orchestrator::TaskStatus::Backlog {
            self.set_status(format!(
                "task is not in backlog (status = {:?})",
                task.status
            ));
            return;
        }
        let display = format!(
            "{} — backlog (task {})",
            p.name,
            crate::orchestrator::short_task_id(&task.task_id),
        );
        self.pending_confirm = Some(PendingConfirm::TaskDelete(PendingTaskDelete {
            project_id: p.id.clone(),
            task_id: task.task_id.clone(),
            display,
            orchestrator_tmux: task.orchestrator_tmux.clone(),
            from_backlog: true,
        }));
        self.view = View::ConfirmClose;
    }

    pub fn take_pending_task_delete(&mut self) -> Option<PendingTaskDelete> {
        let from_backlog = matches!(
            &self.pending_confirm,
            Some(PendingConfirm::TaskDelete(p)) if p.from_backlog
        );
        self.view = if from_backlog {
            View::Backlog
        } else {
            View::Grid
        };
        if matches!(self.pending_confirm, Some(PendingConfirm::TaskDelete(_))) {
            if let Some(PendingConfirm::TaskDelete(p)) = self.pending_confirm.take() {
                return Some(p);
            }
        }
        None
    }

    /// Stage removal of the currently-selected project from the cc-hub
    /// registry, mirroring [`Self::enter_confirm_task_delete`]. Surfaces task
    /// count in the prompt so the user sees how much state they're nuking.
    pub fn enter_confirm_project_delete(&mut self) {
        let Some(p) = self.selected_project().cloned() else {
            self.set_status("no project selected".into());
            return;
        };
        let n = self
            .projects
            .snapshot
            .tasks
            .get(&p.id)
            .map(|v| v.len())
            .unwrap_or(0);
        let display = format!("{} ({} task{})", p.name, n, if n == 1 { "" } else { "s" });
        self.pending_confirm = Some(PendingConfirm::ProjectDelete(PendingProjectDelete {
            project_id: p.id.clone(),
            display,
        }));
        self.view = View::ConfirmClose;
    }

    pub fn take_pending_project_delete(&mut self) -> Option<PendingProjectDelete> {
        self.view = View::Grid;
        if matches!(self.pending_confirm, Some(PendingConfirm::ProjectDelete(_))) {
            if let Some(PendingConfirm::ProjectDelete(p)) = self.pending_confirm.take() {
                return Some(p);
            }
        }
        None
    }

    /// Stage an orchestrator restart behind a confirmation prompt. The
    /// actual restart reloads state at confirmation time; only task identity
    /// and display text are captured here.
    pub fn enter_confirm_task_restart(&mut self) {
        let Some(p) = self.selected_project().cloned() else {
            self.set_status("no project selected".into());
            return;
        };
        let Some(task) = self.selected_project_task().cloned() else {
            self.set_status("no task selected — focus a task on the kanban first".into());
            return;
        };
        let status_label = task.status.as_str();
        let display = format!(
            "{} — {} (task {})",
            p.name,
            status_label,
            crate::orchestrator::short_task_id(&task.task_id),
        );
        self.pending_confirm = Some(PendingConfirm::TaskRestart(PendingTaskRestart {
            project_id: p.id.clone(),
            task_id: task.task_id.clone(),
            display,
        }));
        self.view = View::ConfirmClose;
    }

    pub fn take_pending_task_restart(&mut self) -> Option<PendingTaskRestart> {
        self.view = View::Grid;
        if matches!(self.pending_confirm, Some(PendingConfirm::TaskRestart(_))) {
            if let Some(PendingConfirm::TaskRestart(p)) = self.pending_confirm.take() {
                return Some(p);
            }
        }
        None
    }

    pub fn set_status(&mut self, msg: String) {
        self.status_msg = Some((msg, Instant::now()));
    }

    /// Stamp an ack for the currently-selected session, forcing it to display
    /// as Idle until new activity advances its watermark. Works for any
    /// non-Idle state (WaitingForInput or Processing).
    /// Returns true if an ack was recorded.
    pub fn ack_selected(&mut self) -> bool {
        let Some(session) = self.sessions.selected_session_info() else {
            return false;
        };
        if session.state == SessionState::Idle {
            return false;
        }
        let id = session.session_id.clone();
        let watermark = session.last_activity;
        self.sessions.acks.ack(&id, watermark);
        // Apply immediately so the UI reflects the ack before the next scan tick.
        let (sel_group, sel_in_group) = (self.sessions.sel_group, self.sessions.sel_in_group);
        if let Some(s) = self
            .sessions
            .groups
            .get_mut(sel_group)
            .and_then(|g| g.sessions.get_mut(sel_in_group))
        {
            s.state = SessionState::Idle;
        }
        true
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
        self.sessions.selected_session_id()
    }

    pub fn selected_session_info(&self) -> Option<&SessionInfo> {
        self.sessions.selected_session_info()
    }

    /// Apply a fresh scan snapshot. Returns true when anything the renderer
    /// shows actually changed, so the caller can skip the repaint — and, more
    /// importantly, so unchanged ticks never rewrite the selection.
    ///
    /// Three tiers, cheapest first:
    /// - identical snapshot → no-op;
    /// - same cards in the same slots → swap card contents in place, leaving
    ///   the cursor alone (a state flip updates the badge, it does not
    ///   reshuffle the grid);
    /// - membership changed → full rebuild with restore-selection-by-id and
    ///   the new-session focus jump.
    pub fn update_sessions(&mut self, mut sessions: Vec<SessionInfo>) -> bool {
        let acks_active = !self.sessions.acks.is_empty();
        if acks_active {
            // Apply user acks: if a non-Idle session is still at its acked
            // watermark, downgrade it to Idle. Any advance in last_activity clears
            // the ack inside is_acked(), so the real state takes over next tick.
            for s in &mut sessions {
                if s.state != SessionState::Idle
                    && s.state != SessionState::Inactive
                    && self.sessions.acks.is_acked(&s.session_id, s.last_activity)
                {
                    s.state = SessionState::Idle;
                }
            }
            let live_ids: HashSet<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
            self.sessions.acks.retain_existing(&live_ids);
        }

        self.last_refresh = Instant::now();

        // Resolve task-board agent bindings: a freshly-assigned task knows
        // only its tmux name until the scanner sees the session; learning the
        // session id here is what lets `f` resume after the tmux dies.
        let task_bindings_changed = self.tasks.board.bind_sessions(&sessions);

        let new_groups = self.build_groups(&sessions);
        let same_structure = new_groups.len() == self.sessions.groups.len()
            && new_groups.iter().zip(&self.sessions.groups).all(|(n, o)| {
                n.cwd == o.cwd
                    && n.sessions.len() == o.sessions.len()
                    && n.sessions
                        .iter()
                        .zip(&o.sessions)
                        .all(|(a, b)| a.session_id == b.session_id)
            });
        if same_structure {
            // Same cards in the same slots: refresh their contents and leave
            // the cursor untouched. No restore, no focus jump — a scan tick
            // that changes nothing the user can act on must not move the
            // selection out from under an in-flight keypress.
            let changed = new_groups != self.sessions.groups;
            self.sessions.groups = new_groups;
            self.sessions.last_sessions = sessions;
            if changed && self.view == View::PromptInput {
                self.dispatch_target = Self::compute_dispatch_target(&self.sessions.groups);
            }
            return changed || task_bindings_changed;
        }

        self.sessions.last_sessions = sessions;
        self.adopt_groups(new_groups);

        let current_ids: HashSet<String> = self
            .sessions
            .groups
            .iter()
            .flat_map(|g| g.sessions.iter().map(|s| s.session_id.clone()))
            .collect();
        // First tick seeds known ids without hijacking the cursor; later ticks
        // jump selection to a freshly-appeared session so it gets focus.
        let new_selection = self.sessions.known_session_ids.as_ref().and_then(|known| {
            self.sessions
                .groups
                .iter()
                .enumerate()
                .find_map(|(gi, group)| {
                    group
                        .sessions
                        .iter()
                        .position(|s| !known.contains(&s.session_id))
                        .map(|si| (gi, si))
                })
        });
        self.sessions.known_session_ids = Some(current_ids);
        if let Some((gi, si)) = new_selection {
            log::debug!(
                "scan: new session appeared, focus jump ({}, {}) -> ({}, {})",
                self.sessions.sel_group,
                self.sessions.sel_in_group,
                gi,
                si
            );
            self.sessions.sel_group = gi;
            self.sessions.sel_in_group = si;
        }
        true
    }

    /// Filter, group, and order `sessions` into the rendered group list.
    /// Pure with respect to selection — callers decide whether the result
    /// warrants a selection restore (see [`Self::adopt_groups`]).
    fn build_groups(&self, sessions: &[SessionInfo]) -> Vec<ProjectGroup> {
        let roles = self.projects.snapshot.roles_by_tmux();

        let sessions: Vec<SessionInfo> = sessions
            .iter()
            .filter(|s| self.sessions.show_inactive || s.state != SessionState::Inactive)
            .filter(|s| {
                // Hide tmux sessions claimed by an orchestrator or worker
                // unless the user has asked to see them. Sessions without a
                // tmux name (legacy/manual launches) always show.
                if self.sessions.show_orch_workers {
                    return true;
                }
                match s.tmux_session.as_deref() {
                    Some(name) => !roles.contains_key(name),
                    None => true,
                }
            })
            .cloned()
            .collect();

        // Group sessions by cwd. The scanner pre-sorts by stable keys
        // (started_at desc, session id), and HashMap::entry preserves
        // bucket-relative order, so each group comes out sorted — and the
        // order never depends on volatile session state, so an ack downgrade
        // or a state flip can't reshuffle cards under the cursor.
        let mut group_map: HashMap<String, Vec<SessionInfo>> = HashMap::new();
        for s in sessions {
            group_map.entry(s.cwd.clone()).or_default().push(s);
        }

        let mut groups: Vec<ProjectGroup> = group_map
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
        groups.sort_by_key(|a| a.name.to_lowercase());
        groups
    }

    /// Install a freshly-built group list and re-anchor the selection on the
    /// session id that was selected before, clamping when it's gone.
    fn adopt_groups(&mut self, groups: Vec<ProjectGroup>) {
        let prev_id = self.selected_session_id();
        let sel_before = (self.sessions.sel_group, self.sessions.sel_in_group);
        self.sessions.groups = groups;

        if self.view == View::PromptInput {
            self.dispatch_target = Self::compute_dispatch_target(&self.sessions.groups);
        }

        // Re-anchor the selection on the previously-selected session id;
        // clamp into range when it's gone.
        let restored = prev_id.and_then(|id| {
            self.sessions
                .groups
                .iter()
                .enumerate()
                .find_map(|(gi, group)| {
                    group
                        .sessions
                        .iter()
                        .position(|s| s.session_id == id)
                        .map(|si| (gi, si))
                })
        });
        match restored {
            Some((gi, si)) => {
                self.sessions.sel_group = gi;
                self.sessions.sel_in_group = si;
            }
            None if self.sessions.groups.is_empty() => {
                self.sessions.sel_group = 0;
                self.sessions.sel_in_group = 0;
            }
            None => {
                self.sessions.sel_group =
                    self.sessions.sel_group.min(self.sessions.groups.len() - 1);
                let max_in = self.sessions.groups[self.sessions.sel_group]
                    .sessions
                    .len()
                    .saturating_sub(1);
                self.sessions.sel_in_group = self.sessions.sel_in_group.min(max_in);
            }
        }

        // Background selection rewrites are the prime suspect whenever "my
        // keypress did nothing" gets reported — make every one traceable.
        let sel_after = (self.sessions.sel_group, self.sessions.sel_in_group);
        if sel_before != sel_after {
            log::debug!(
                "scan: rebuild moved selection {:?} -> {:?} ({})",
                sel_before,
                sel_after,
                if restored.is_some() {
                    "id follow"
                } else {
                    "clamp"
                }
            );
        }
    }

    fn rebuild_groups(&mut self) {
        let groups = self.build_groups(&self.sessions.last_sessions);
        self.adopt_groups(groups);
    }

    pub fn update_detail(&mut self, detail: SessionDetail) {
        self.detail = Some(detail);
        self.detail_loading = false;
    }

    pub fn update_grid_cols(&mut self, width: u16) {
        let cell_width = config::get().ui.cell_width.max(1);
        self.sessions.grid_cols = (width / cell_width).max(1);
    }

    pub fn session_count(&self) -> usize {
        self.sessions.session_count()
    }

    pub fn attention_count(&self) -> usize {
        self.sessions.attention_count()
    }

    pub fn log_state_dump(&self) {
        log::info!("=== state dump on quit ===");
        log::info!(
            "view={:?} sel_group={} sel_in_group={} grid_cols={} groups={} sessions={} attention={}",
            self.view,
            self.sessions.sel_group,
            self.sessions.sel_in_group,
            self.sessions.grid_cols,
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
        if let Some((target_pid, name, tmux)) = &self.dispatch_target {
            log::info!(
                "dispatch_target: pid={} project={} tmux={}",
                target_pid,
                name,
                tmux
            );
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

    /// Open the Projects "Result" popup for the currently-selected task.
    /// Returns false when no task is selected, so the caller can surface a
    /// status-bar hint instead of opening an empty popup.
    pub fn enter_projects_result(&mut self) -> bool {
        if self.selected_project_task().is_none() {
            return false;
        }
        self.projects.result_artifact_sel = 0;
        self.projects.result_scroll = 0;
        self.projects.result_artifact_expanded = false;
        self.view = View::ProjectsResult;
        true
    }

    pub fn close_projects_result(&mut self) {
        self.view = View::Grid;
        self.projects.result_artifact_sel = 0;
        self.projects.result_scroll = 0;
        self.projects.result_artifact_expanded = false;
    }

    /// The artifact under the popup cursor, if any. Used by the `c` and `o`
    /// keybinds to know what path to act on.
    pub fn selected_result_artifact(&self) -> Option<&crate::orchestrator::Artifact> {
        self.projects.selected_result_artifact()
    }
}

pub fn kanban_col_name(col: usize) -> &'static str {
    match col {
        0 => "Planning",
        1 => "Running",
        2 => "Review",
        3 => "Merging",
        _ => "Done",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::{Project, TaskState, TaskStatus, Worker};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn project(id: &str) -> Project {
        Project {
            id: id.to_string(),
            name: id.to_string(),
            root: PathBuf::from(format!("/tmp/{}", id)),
            created_at: 0,
            build_cmd: None,
        }
    }

    fn task(project_id: &str, task_id: &str, status: TaskStatus, with_worker: bool) -> TaskState {
        let mut t = TaskState::new(
            project_id.to_string(),
            PathBuf::from(format!("/tmp/{}", project_id)),
            String::new(),
        );
        t.task_id = task_id.to_string();
        t.status = status;
        if with_worker {
            t.workers.push(Worker {
                tmux_name: "w-1".to_string(),
                cwd: PathBuf::from("/tmp/w"),
                worktree: None,
                readonly: false,
                spawned_at: 0,
                agent_id: "claude".to_string(),
                agent_kind: crate::agent::AgentKind::Claude,
            });
        }
        t
    }

    fn snapshot(p: Project, tasks: Vec<TaskState>) -> ProjectsSnapshot {
        snapshot_many(vec![(p, tasks)])
    }

    fn snapshot_many(projects: Vec<(Project, Vec<TaskState>)>) -> ProjectsSnapshot {
        let mut snap = ProjectsSnapshot::empty();
        for (p, tasks) in projects {
            let pid = p.id.clone();
            snap.projects.push(p);
            snap.tasks
                .insert(pid, tasks.into_iter().map(Arc::new).collect());
        }
        snap
    }

    fn fake_session(tmux: &str, state: SessionState) -> SessionInfo {
        SessionInfo {
            agent_id: "claude".into(),
            agent_kind: crate::agent::AgentKind::Claude,
            pid: 1,
            session_id: tmux.into(),
            cwd: "/tmp".into(),
            project_name: "tmp".into(),
            started_at: 0,
            last_activity: None,
            state,
            last_user_message: None,
            summary: None,
            title: None,
            titling: false,
            model: None,
            git_branch: None,
            version: None,
            jsonl_path: None,
            tmux_session: Some(tmux.into()),
            current_tool: None,
            is_thinking: false,
            context_tokens: None,
            tool_uses_count: 0,
        }
    }

    #[test]
    fn pending_dispatch_is_fifo_queue() {
        let mut app = App::new();
        app.queue_pending_dispatch("tmux-a".into(), "prompt-a".into());
        app.queue_pending_dispatch("tmux-b".into(), "prompt-b".into());
        for pd in &mut app.pending_dispatch {
            pd.queued_at = Instant::now() - Duration::from_secs(6);
        }
        app.sessions.last_sessions = vec![
            fake_session("tmux-a", SessionState::Idle),
            fake_session("tmux-b", SessionState::Idle),
        ];

        match app.poll_pending_dispatch() {
            DispatchAction::Send { tmux, prompt } => {
                assert_eq!(tmux, "tmux-a");
                assert_eq!(prompt, "prompt-a");
            }
            _ => panic!("first queued dispatch should send"),
        }
        match app.poll_pending_dispatch() {
            DispatchAction::Send { tmux, prompt } => {
                assert_eq!(tmux, "tmux-b");
                assert_eq!(prompt, "prompt-b");
            }
            _ => panic!("second queued dispatch should send"),
        }
    }

    #[test]
    fn projects_cursor_follows_task_across_status_transition() {
        let mut app = App::new();
        app.current_tab = Tab::Projects;

        let p = project("p-1");
        // Running + workers → kanban column 1 (true Running).
        let snap1 = snapshot(
            p.clone(),
            vec![task("p-1", "t-1", TaskStatus::Running, true)],
        );
        app.update_projects(snap1);

        assert_eq!(app.projects.col, 1, "Running+workers should land in col 1");
        assert_eq!(
            app.selected_project_task().map(|t| t.task_id.clone()),
            Some("t-1".to_string()),
        );

        // Same task moves to Review (column 2). Cursor must follow.
        let snap2 = snapshot(p, vec![task("p-1", "t-1", TaskStatus::Review, false)]);
        app.update_projects(snap2);

        assert_eq!(
            app.projects.col, 2,
            "cursor should follow t-1 into the Review column"
        );
        assert_eq!(
            app.selected_project_task().map(|t| t.task_id.clone()),
            Some("t-1".to_string()),
            "selected task should still be t-1",
        );
    }

    #[test]
    fn approve_review_transitions_task_to_merging() {
        use crate::test_util::HOME_TEST_LOCK;
        let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let p = project("p-app");
        let mut t = task("p-app", "t-app", TaskStatus::Review, false);
        t.project_root = home.path().join("repo");
        std::fs::create_dir_all(&t.project_root).unwrap();
        crate::orchestrator::write_task_state(&t).expect("write task");

        // Hand-write a PR record so approve_review_task() can read it.
        let pr = crate::pr::PullRequest {
            id: 7,
            task_id: "t-app".into(),
            project_id: "p-app".into(),
            branch: "cc-hub/t-app-feat".into(),
            base: "main".into(),
            title: "x".into(),
            description: "x".into(),
            review_state: crate::pr::ReviewState::Open,
            comments: vec![],
            approved_at_branch_sha: None,
            approved_at_base_sha: None,
            created_at: 0,
            updated_at: 0,
        };
        crate::pr::write_pr(&pr).expect("write pr");

        let mut app = App::new();
        app.current_tab = Tab::Projects;
        app.update_projects(snapshot(p, vec![t]));
        // Cursor lands on the Review task (col 2, row 0).
        assert_eq!(app.projects.col, 2);

        assert_eq!(app.approve_review_task(), ApproveOutcome::PrApproved);

        // Reload and verify status transitioned.
        let reloaded = crate::orchestrator::read_task_state("p-app", "t-app").expect("read");
        assert_eq!(reloaded.status, TaskStatus::Merging);
        let pr_after = crate::pr::read_pr("p-app", "t-app")
            .expect("read pr")
            .expect("present");
        assert_eq!(pr_after.review_state, crate::pr::ReviewState::Approved);

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn approve_review_pr_less_task_transitions_to_done() {
        use crate::test_util::HOME_TEST_LOCK;
        let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let p = project("p-noPR");
        let mut t = task("p-noPR", "t-noPR", TaskStatus::Review, false);
        t.project_root = home.path().join("repo");
        std::fs::create_dir_all(&t.project_root).unwrap();
        crate::orchestrator::write_task_state(&t).expect("write task");
        // Deliberately do NOT write a pr.json — this is the PR-less case.

        let mut app = App::new();
        app.current_tab = Tab::Projects;
        app.update_projects(snapshot(p, vec![t]));
        assert_eq!(app.projects.col, 2);

        assert_eq!(app.approve_review_task(), ApproveOutcome::DoneNoPr);

        let reloaded = crate::orchestrator::read_task_state("p-noPR", "t-noPR").expect("read");
        assert_eq!(reloaded.status, TaskStatus::Done);
        assert!(crate::pr::read_pr("p-noPR", "t-noPR")
            .expect("read pr")
            .is_none());

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn backlog_tasks_absent_from_every_kanban_column() {
        let mut app = App::new();
        app.current_tab = Tab::Projects;

        let p = project("p-bl");
        // One task in each column-producing status, plus two Backlog tasks.
        let tasks = vec![
            task("p-bl", "t-plan", TaskStatus::Running, false), // col 0 (Planning)
            task("p-bl", "t-run", TaskStatus::Running, true),   // col 1 (Running)
            task("p-bl", "t-rev", TaskStatus::Review, false),   // col 2 (Review)
            task("p-bl", "t-mrg", TaskStatus::Merging, false),  // col 3 (Merging)
            task("p-bl", "t-done", TaskStatus::Done, false),    // col 4 (Done)
            task("p-bl", "t-bl1", TaskStatus::Backlog, false),
            task("p-bl", "t-bl2", TaskStatus::Backlog, true),
        ];
        app.update_projects(snapshot(p, tasks));

        // No backlog task may appear in any of the five kanban columns.
        for col in 0..5 {
            for t in app.kanban_column_tasks(col) {
                assert_ne!(
                    t.status,
                    TaskStatus::Backlog,
                    "backlog task {} leaked into column {}",
                    t.task_id,
                    col
                );
            }
        }

        // They live only in the Backlog popup's task list.
        let backlog: Vec<_> = app
            .backlog_tasks()
            .iter()
            .map(|t| t.task_id.clone())
            .collect();
        assert_eq!(backlog, vec!["t-bl1".to_string(), "t-bl2".to_string()]);
    }

    #[test]
    fn rollback_merging_to_review_restores_actionable_status() {
        use crate::test_util::HOME_TEST_LOCK;
        let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        // A task stranded in Merging (e.g. approve wrote Merging but the
        // orchestrator turned out to be dead).
        let mut t = task("p-rb", "t-rb", TaskStatus::Merging, false);
        t.project_root = home.path().join("repo");
        std::fs::create_dir_all(&t.project_root).unwrap();
        crate::orchestrator::write_task_state(&t).expect("write task");

        let mut app = App::new();
        app.rollback_merging_to_review("p-rb", "t-rb");

        let reloaded = crate::orchestrator::read_task_state("p-rb", "t-rb").expect("read");
        assert_eq!(reloaded.status, TaskStatus::Review);
        assert!(reloaded
            .note
            .as_deref()
            .is_some_and(|n| n.contains("resurrect")));

        // Idempotent: a non-Merging task is left untouched.
        app.rollback_merging_to_review("p-rb", "t-rb");
        let again = crate::orchestrator::read_task_state("p-rb", "t-rb").expect("read");
        assert_eq!(again.status, TaskStatus::Review);

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn projects_cursor_clamps_when_task_disappears() {
        let mut app = App::new();
        app.current_tab = Tab::Projects;

        let p = project("p-1");
        let snap1 = snapshot(
            p.clone(),
            vec![task("p-1", "t-1", TaskStatus::Running, true)],
        );
        app.update_projects(snap1);
        assert_eq!(app.projects.col, 1);

        // Task vanishes from the snapshot entirely.
        let snap2 = snapshot(p, Vec::new());
        app.update_projects(snap2);

        assert!(app.selected_project_task().is_none());
        assert_eq!(
            app.projects.col, 1,
            "column should stay where it was when task disappears",
        );
        assert_eq!(app.projects.task_sel, 0, "row should clamp to 0");
    }

    #[test]
    fn project_chip_cycle_wraps_and_clamps_to_new_project_tasks() {
        let mut app = App::new();
        app.current_tab = Tab::Projects;

        let p1 = project("p-1");
        let p2 = project("p-2");
        app.update_projects(snapshot_many(vec![
            (p1, vec![task("p-1", "t-review", TaskStatus::Review, false)]),
            (p2, vec![task("p-2", "t-plan", TaskStatus::Running, false)]),
        ]));

        assert_eq!(app.projects.sel, 0);
        assert_eq!(app.projects.col, 2, "first project starts in Review");
        assert_eq!(
            app.selected_project_task().map(|t| t.task_id.as_str()),
            Some("t-review")
        );

        app.projects.move_down();
        assert_eq!(app.projects.sel, 1);
        assert_eq!(
            app.projects.col, 0,
            "switching projects should jump from empty Review to Planning"
        );
        assert_eq!(
            app.selected_project_task().map(|t| t.task_id.as_str()),
            Some("t-plan")
        );

        app.projects.move_down();
        assert_eq!(app.projects.sel, 0, "L/]/down wraps to first project");
        assert_eq!(app.projects.col, 2);

        app.projects.move_up();
        assert_eq!(app.projects.sel, 1, "H/[/up wraps to last project");
        assert_eq!(app.projects.col, 0);
    }

    #[test]
    fn pending_focus_jumps_to_planning_when_task_appears() {
        let mut app = App::new();
        app.current_tab = Tab::Projects;
        let p = project("p-1");
        // Initial snapshot: empty (the task hasn't been written yet from
        // the orchestrator's POV, or is still in Backlog).
        app.update_projects(snapshot(p.clone(), Vec::new()));
        app.projects.pending_focus_task_id = Some("t-new".to_string());
        app.projects.pending_focus_budget = 5;
        // New snapshot: task appears as Running with no workers → Planning column.
        app.update_projects(snapshot(
            p,
            vec![task("p-1", "t-new", TaskStatus::Running, false)],
        ));
        assert_eq!(app.projects.col, 0, "cursor should land on Planning");
        assert_eq!(app.projects.task_sel, 0);
        assert!(
            app.projects.pending_focus_task_id.is_none(),
            "pending should clear after success"
        );
    }

    #[test]
    fn pending_focus_budget_runs_out_when_task_never_arrives() {
        let mut app = App::new();
        app.current_tab = Tab::Projects;
        let p = project("p-1");
        app.update_projects(snapshot(p.clone(), Vec::new()));
        app.projects.pending_focus_task_id = Some("t-ghost".to_string());
        app.projects.pending_focus_budget = 2;
        // Two empty snapshots → budget exhausted, pending cleared.
        app.update_projects(snapshot(p.clone(), Vec::new()));
        assert!(
            app.projects.pending_focus_task_id.is_some(),
            "still pending after 1"
        );
        app.update_projects(snapshot(p, Vec::new()));
        assert!(
            app.projects.pending_focus_task_id.is_none(),
            "cleared after budget=0"
        );
    }

    /// Opening the to-do panel must pick up edits that landed on disk while
    /// it was closed (another instance, the file hand-edited) — App::new()
    /// deliberately starts with an empty list and defers I/O to panel open.
    #[test]
    #[cfg(unix)]
    fn enter_todo_panel_reloads_from_disk() {
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            assert!(app.todo.list.is_empty(), "no disk I/O in App::new()");
            // Simulate an external writer landing while the panel is closed.
            let mut external = crate::todo::TodoList::load();
            external.add("written elsewhere");
            app.enter_todo_panel();
            assert_eq!(app.todo.list.len(), 1);
            assert_eq!(app.todo.list.items()[0].text, "written elsewhere");
        });
    }

    /// Three sessions in one group, in scanner order. `fake_session` keys
    /// session_id off the tmux name, so ids are the given names.
    fn seed_three(app: &mut App) {
        let changed = app.update_sessions(vec![
            fake_session("a", SessionState::Idle),
            fake_session("b", SessionState::Idle),
            fake_session("c", SessionState::Idle),
        ]);
        assert!(changed, "first snapshot is a structure change");
    }

    fn grid_ids(app: &App) -> Vec<String> {
        app.sessions.groups[0]
            .sessions
            .iter()
            .map(|s| s.session_id.clone())
            .collect()
    }

    #[test]
    fn state_flip_updates_card_in_place_without_touching_selection() {
        let mut app = App::new();
        seed_three(&mut app);
        app.sessions.move_right();
        assert_eq!(app.selected_session_id().as_deref(), Some("b"));

        // b flips state: same cards, same slots — content-only update.
        let changed = app.update_sessions(vec![
            fake_session("a", SessionState::Idle),
            fake_session("b", SessionState::Processing),
            fake_session("c", SessionState::Idle),
        ]);
        assert!(changed, "a state flip is a visible change");
        assert_eq!(grid_ids(&app), ["a", "b", "c"], "order must not change");
        assert_eq!(app.sessions.sel_in_group, 1, "cursor must not move");
        assert_eq!(
            app.sessions.groups[0].sessions[1].state,
            SessionState::Processing,
            "card content must refresh"
        );
    }

    #[test]
    fn identical_snapshot_reports_no_change_and_keeps_selection() {
        let mut app = App::new();
        seed_three(&mut app);
        app.sessions.move_right();

        let changed = app.update_sessions(vec![
            fake_session("a", SessionState::Idle),
            fake_session("b", SessionState::Idle),
            fake_session("c", SessionState::Idle),
        ]);
        assert!(!changed, "identical snapshot must not request a repaint");
        assert_eq!(app.sessions.sel_in_group, 1, "cursor must not move");
    }

    #[test]
    fn membership_change_rebuilds_and_follows_selected_id() {
        let mut app = App::new();
        seed_three(&mut app);
        app.sessions.move_right();
        assert_eq!(app.selected_session_id().as_deref(), Some("b"));

        // a disappears: structure change → rebuild, selection follows b's id
        // to its new slot.
        let changed = app.update_sessions(vec![
            fake_session("b", SessionState::Idle),
            fake_session("c", SessionState::Idle),
        ]);
        assert!(changed);
        assert_eq!(grid_ids(&app), ["b", "c"]);
        assert_eq!(app.selected_session_id().as_deref(), Some("b"));
        assert_eq!(app.sessions.sel_in_group, 0);
    }

    #[test]
    fn update_projects_identical_snapshot_reports_no_change() {
        let mut app = App::new();
        let p = project("p-1");
        let t = task("p-1", "t-1", TaskStatus::Running, false);
        assert!(app.update_projects(snapshot(p.clone(), vec![t.clone()])));
        assert!(
            !app.update_projects(snapshot(p, vec![t])),
            "identical projects snapshot must not request a repaint"
        );
    }

    // `$HOME`-redirected (TaskBoard/Bookmarks persist on mutation), so
    // unix-only like the other with_temp_home suites.
    #[cfg(unix)]
    mod assign_picker {
        use super::*;
        use crate::folder_picker::PlaceSource;
        use crate::test_util::with_temp_home;

        #[test]
        fn known_places_orders_projects_bookmarks_recents_and_dedups() {
            with_temp_home(|| {
                let mut app = App::new();
                app.projects.snapshot = snapshot(project("cc-hub"), vec![]);
                app.bookmarks.toggle(PathBuf::from("/tmp/bm"));
                // Bookmark duplicating the project root must be swallowed.
                app.bookmarks.toggle(PathBuf::from("/tmp/cc-hub"));
                let id = app.tasks.board.add("t").unwrap();
                app.tasks
                    .board
                    .assign(&id, "/tmp/recent", "claude", "mux-1");

                let places = app.known_places();
                let got: Vec<(&str, PlaceSource)> =
                    places.iter().map(|p| (p.name.as_str(), p.source)).collect();
                assert_eq!(
                    got,
                    vec![
                        ("cc-hub", PlaceSource::Project),
                        ("bm", PlaceSource::Bookmark),
                        ("recent", PlaceSource::Recent),
                    ]
                );
            });
        }

        #[test]
        fn assign_picker_opens_places_and_preselects_previous_cwd() {
            with_temp_home(|| {
                let mut app = App::new();
                app.projects.snapshot =
                    snapshot_many(vec![(project("p-a"), vec![]), (project("p-b"), vec![])]);
                let id = app.tasks.board.add("do the thing").unwrap();
                // A previous assignment on p-b: reopening the picker must
                // land the cursor there, not on the first candidate.
                app.tasks.board.assign(&id, "/tmp/p-b", "claude", "mux-1");
                app.focus_task(&id);

                assert!(app.enter_task_assign_picker());
                let picker = app.folder_picker.as_ref().unwrap();
                assert_eq!(picker.mode, PickerMode::Places);
                assert_eq!(picker.selected_place().unwrap().name, "p-b");
                assert_eq!(app.tasks.pending_assign.as_deref(), Some(id.as_str()));
            });
        }

        #[test]
        fn assign_picker_promotes_last_assigned_project_to_front() {
            with_temp_home(|| {
                let mut app = App::new();
                app.projects.snapshot =
                    snapshot_many(vec![(project("p-a"), vec![]), (project("p-b"), vec![])]);
                // An earlier task went to p-b: a fresh task's picker must
                // open with p-b first and selected, ready for plain Enter.
                let prev = app.tasks.board.add("earlier").unwrap();
                app.tasks.board.assign(&prev, "/tmp/p-b", "claude", "mux-1");
                let id = app.tasks.board.add("next").unwrap();
                app.focus_task(&id);

                assert!(app.enter_task_assign_picker());
                let picker = app.folder_picker.as_ref().unwrap();
                let names: Vec<&str> = picker
                    .rows
                    .iter()
                    .map(|r| picker.places[r.place].name.as_str())
                    .collect();
                assert_eq!(names, vec!["p-b", "p-a"]);
                assert_eq!(picker.selected_place().unwrap().name, "p-b");
            });
        }

        #[test]
        fn assign_picker_resurrects_last_cwd_missing_from_places() {
            with_temp_home(|| {
                let mut app = App::new();
                app.projects.snapshot = snapshot(project("p-a"), vec![]);
                // The assigned task is deleted, so /tmp/gone is in no
                // project/bookmark/recent — it must still lead the list.
                let prev = app.tasks.board.add("earlier").unwrap();
                app.tasks
                    .board
                    .assign(&prev, "/tmp/gone", "claude", "mux-1");
                app.tasks.board.remove(&prev);
                let id = app.tasks.board.add("next").unwrap();
                app.focus_task(&id);

                assert!(app.enter_task_assign_picker());
                let picker = app.folder_picker.as_ref().unwrap();
                let first = picker.selected_place().unwrap();
                assert_eq!(first.name, "gone");
                assert_eq!(first.source, PlaceSource::Recent);
            });
        }

        #[test]
        fn assign_picker_falls_back_to_browse_when_nothing_known() {
            with_temp_home(|| {
                let mut app = App::new();
                let id = app.tasks.board.add("t").unwrap();
                app.focus_task(&id);
                assert!(app.enter_task_assign_picker());
                assert_eq!(app.folder_picker.as_ref().unwrap().mode, PickerMode::Browse);
            });
        }

        #[test]
        fn tab_toggles_between_places_and_browse_in_assign_and_session_flows() {
            with_temp_home(|| {
                let mut app = App::new();
                app.projects.snapshot = snapshot(project("p-a"), vec![]);
                let id = app.tasks.board.add("t").unwrap();
                app.focus_task(&id);
                assert!(app.enter_task_assign_picker());
                let mode = |app: &App| app.folder_picker.as_ref().unwrap().mode;
                assert_eq!(mode(&app), PickerMode::Places);
                app.toggle_places_picker_mode();
                assert_eq!(mode(&app), PickerMode::Browse);
                app.toggle_places_picker_mode();
                assert_eq!(mode(&app), PickerMode::Places);

                // The sessions-tab `N` flow toggles the same way.
                app.close_folder_picker();
                app.enter_session_places_picker();
                assert_eq!(mode(&app), PickerMode::Places);
                app.toggle_places_picker_mode();
                assert_eq!(mode(&app), PickerMode::Browse);
                app.toggle_places_picker_mode();
                assert_eq!(mode(&app), PickerMode::Places);

                // The Projects flows keep their plain browser: no flip.
                app.close_folder_picker();
                app.enter_folder_picker_for_register_only();
                app.toggle_places_picker_mode();
                assert_eq!(mode(&app), PickerMode::Browse);
            });
        }

        #[test]
        fn session_places_picker_opens_places_without_pending_assign() {
            with_temp_home(|| {
                let mut app = App::new();
                app.projects.snapshot = snapshot(project("p-a"), vec![]);
                app.enter_session_places_picker();
                let picker = app.folder_picker.as_ref().unwrap();
                assert_eq!(picker.mode, PickerMode::Places);
                assert!(app.tasks.pending_assign.is_none());
                assert_eq!(app.view, View::FolderPicker);
            });
        }

        #[test]
        fn session_places_picker_falls_back_to_browse_when_nothing_known() {
            with_temp_home(|| {
                let mut app = App::new();
                app.enter_session_places_picker();
                assert_eq!(app.folder_picker.as_ref().unwrap().mode, PickerMode::Browse);
            });
        }

        #[test]
        fn assign_places_does_not_promote_home_quick_assign() {
            with_temp_home(|| {
                let mut app = App::new();
                app.projects.snapshot =
                    snapshot_many(vec![(project("p-a"), vec![]), (project("p-b"), vec![])]);
                let home = dirs::home_dir().unwrap().display().to_string();
                let prev = app.tasks.board.add("broad question").unwrap();
                app.tasks.board.assign(&prev, &home, "claude", "mux-1");
                let id = app.tasks.board.add("next").unwrap();
                app.focus_task(&id);

                assert!(app.enter_task_assign_picker());
                let picker = app.folder_picker.as_ref().unwrap();
                // $HOME must neither lead the list nor appear resurrected.
                assert_eq!(picker.selected_place().unwrap().name, "p-a");
            });
        }

        #[test]
        fn quick_assign_at_home_requires_focused_undone_task() {
            with_temp_home(|| {
                let mut app = App::new();
                // No task focused: refuse rather than spawn.
                assert!(app.assign_selected_task_at_home().is_none());
                let id = app.tasks.board.add("t").unwrap();
                app.tasks.board.set_status(&id, TaskItemStatus::Done);
                app.focus_task(&id);
                assert!(app.assign_selected_task_at_home().is_none());
                assert!(app.tasks.pending_assign.is_none());
            });
        }
    }

    // `$HOME`-redirected like assign_picker (the board persists on
    // mutation), so unix-only.
    #[cfg(unix)]
    mod task_rename {
        use super::*;
        use crate::test_util::with_temp_home;

        #[test]
        fn rename_prefills_commits_in_place_and_empty_keeps_text() {
            with_temp_home(|| {
                let mut app = App::new();
                // Nothing focused: refuse instead of opening an empty popup.
                assert!(!app.enter_task_rename());

                let id = app.tasks.board.add("old text").unwrap();
                app.focus_task(&id);
                assert!(app.enter_task_rename());
                assert_eq!(app.view, View::TaskInput);
                assert_eq!(app.tasks.input, "old text");

                app.tasks.input = "new text".into();
                assert!(app.submit_task_input());
                assert_eq!(app.tasks.board.get(&id).unwrap().text, "new text");
                assert_eq!(app.view, View::Grid);
                assert!(app.tasks.renaming.is_none());

                // A whitespace-only commit must not wipe the task.
                assert!(app.enter_task_rename());
                app.tasks.input = "   ".into();
                assert!(!app.submit_task_input());
                assert_eq!(app.tasks.board.get(&id).unwrap().text, "new text");
            });
        }

        #[test]
        fn rename_does_not_touch_status_or_binding() {
            with_temp_home(|| {
                let mut app = App::new();
                let id = app.tasks.board.add("t").unwrap();
                app.tasks.board.assign(&id, "/tmp/proj", "claude", "mux-1");
                app.focus_task(&id);
                assert!(app.enter_task_rename());
                app.tasks.input = "sharper wording".into();
                assert!(app.submit_task_input());
                let t = app.tasks.board.get(&id).unwrap();
                assert_eq!(t.text, "sharper wording");
                assert_eq!(t.status, TaskItemStatus::Planning);
                assert_eq!(t.tmux.as_deref(), Some("mux-1"));
            });
        }
    }

    #[cfg(unix)]
    mod board_order {
        use super::*;
        use crate::test_util::with_temp_home;

        // Assignment lands cards in Planning, so the float tests inspect that
        // column; Planning sorts exactly like In Progress.
        fn column_order(app: &App) -> Vec<String> {
            app.task_column(TaskItemStatus::Planning)
                .iter()
                .map(|t| t.id.clone())
                .collect()
        }

        #[test]
        fn live_columns_float_needs_input_and_cursor_follows() {
            with_temp_home(|| {
                let mut app = App::new();
                let a = app.tasks.board.add("a").unwrap();
                let b = app.tasks.board.add("b").unwrap();
                let c = app.tasks.board.add("c").unwrap();
                app.tasks.board.assign(&a, "/tmp", "claude", "mux-a");
                app.tasks.board.assign(&b, "/tmp", "claude", "mux-b");
                app.tasks.board.assign(&c, "/tmp", "claude", "mux-c");
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
                    .task_column(TaskItemStatus::Planning)
                    .iter()
                    .map(|t| t.id.as_str())
                    .collect();
                assert_eq!(order, vec![b.as_str(), c.as_str(), a.as_str()]);

                // The same ordering applies once the cards are promoted.
                for id in [&a, &b, &c] {
                    app.tasks.board.set_status(id, TaskItemStatus::InProgress);
                }
                let order: Vec<&str> = app
                    .task_column(TaskItemStatus::InProgress)
                    .iter()
                    .map(|t| t.id.as_str())
                    .collect();
                assert_eq!(order, vec![b.as_str(), c.as_str(), a.as_str()]);

                // Selection and focus resolve against the rendered order,
                // not the board file's insertion order. Derive In Progress's
                // column index so the test holds whether or not the optional
                // Planning column is configured in.
                let ip_col = visible_task_columns()
                    .iter()
                    .position(|s| *s == TaskItemStatus::InProgress)
                    .unwrap();
                app.focus_task(&a);
                assert_eq!((app.tasks.col, app.tasks.row), (ip_col, 2));
                assert_eq!(app.selected_board_task().unwrap().id, a);
            });
        }

        #[test]
        fn space_on_planning_without_session_stays_planning() {
            with_temp_home(|| {
                let mut app = App::new();
                let id = app.tasks.board.add("plan me").unwrap();
                app.tasks.board.assign(&id, "/tmp", "claude", "mux-dead");
                app.focus_task(&id);
                // No live mux and no resolved session id: nothing to deliver
                // the proceed prompt to, so the card must not move — an In
                // Progress card with no agent working it would be a lie.
                let msg = app.task_space_action().unwrap();
                assert!(msg.contains("press s to re-assign"), "msg: {msg}");
                assert_eq!(
                    app.tasks.board.get(&id).unwrap().status,
                    TaskItemStatus::Planning
                );
            });
        }

        #[test]
        fn scan_state_flips_do_not_reorder_under_the_cursor() {
            with_temp_home(|| {
                let mut app = App::new();
                let a = app.tasks.board.add("a").unwrap();
                let b = app.tasks.board.add("b").unwrap();
                app.tasks.board.assign(&a, "/tmp", "claude", "mux-a");
                app.tasks.board.assign(&b, "/tmp", "claude", "mux-b");
                app.sessions.last_sessions = vec![
                    fake_session("mux-a", SessionState::Processing),
                    fake_session("mux-b", SessionState::WaitingForInput),
                ];
                app.refresh_in_progress_order();
                assert_eq!(column_order(&app), vec![b.clone(), a.clone()]);
                app.focus_task(&a);

                // A scan tick flips both states. The frozen order (and so
                // the card under the cursor) must not move until the tab is
                // re-entered — live re-sorting is how the Sessions grid used
                // to swap cards under the cursor.
                app.sessions.last_sessions = vec![
                    fake_session("mux-a", SessionState::Question),
                    fake_session("mux-b", SessionState::Idle),
                ];
                assert_eq!(column_order(&app), vec![b.clone(), a.clone()]);
                assert_eq!(app.selected_board_task().unwrap().id, a);

                // A task assigned mid-tab joins below the frozen order
                // instead of re-shuffling it.
                let c = app.tasks.board.add("c").unwrap();
                app.tasks.board.assign(&c, "/tmp", "claude", "mux-c");
                assert_eq!(column_order(&app), vec![b, a, c]);
            });
        }

        #[test]
        fn tab_reentry_refloats_and_keeps_cursor_on_the_same_task() {
            with_temp_home(|| {
                let mut app = App::new();
                let a = app.tasks.board.add("a").unwrap();
                let b = app.tasks.board.add("b").unwrap();
                app.tasks.board.assign(&a, "/tmp", "claude", "mux-a");
                app.tasks.board.assign(&b, "/tmp", "claude", "mux-b");
                app.set_tab(Tab::Tasks);
                assert_eq!(column_order(&app), vec![a.clone(), b.clone()]);
                app.focus_task(&b);

                // While the user is elsewhere, b's agent blocks on input;
                // coming back re-floats the column and follows b to its new
                // row instead of leaving the cursor parked on a's card.
                app.set_tab(Tab::Sessions);
                app.sessions.last_sessions =
                    vec![fake_session("mux-b", SessionState::WaitingForInput)];
                app.set_tab(Tab::Tasks);
                assert_eq!(column_order(&app), vec![b.clone(), a]);
                assert_eq!((app.tasks.col, app.tasks.row), (1, 0));
                assert_eq!(app.selected_board_task().unwrap().id, b);
            });
        }

        #[test]
        fn columns_sort_by_priority_stable_within_level() {
            with_temp_home(|| {
                let mut app = App::new();
                // All start at the default P3 and keep insertion order.
                let a = app.tasks.board.add("a").unwrap();
                let b = app.tasks.board.add("b").unwrap();
                let c = app.tasks.board.add("c").unwrap();
                let d = app.tasks.board.add("d").unwrap();
                app.tasks.board.set_priority(&c, TaskPriority::P1);
                app.tasks.board.set_priority(&d, TaskPriority::P2);

                // P1, then P2, then the untouched P3s in their original order.
                let order: Vec<String> = app
                    .task_column(TaskItemStatus::Todo)
                    .iter()
                    .map(|t| t.id.clone())
                    .collect();
                assert_eq!(order, vec![c, d, a, b]);
            });
        }

        #[test]
        fn raising_priority_moves_card_and_cursor_follows() {
            with_temp_home(|| {
                let mut app = App::new();
                app.tasks.board.add("a").unwrap();
                app.tasks.board.add("b").unwrap();
                let c = app.tasks.board.add("c").unwrap();
                app.focus_task(&c);
                assert_eq!((app.tasks.col, app.tasks.row), (0, 2));

                // Bumping c to P1 floats it to the top; the cursor rides along.
                app.set_selected_task_priority(TaskPriority::P1);
                assert_eq!((app.tasks.col, app.tasks.row), (0, 0));
                assert_eq!(app.selected_board_task().unwrap().id, c);
            });
        }

        #[test]
        fn priority_outranks_in_progress_needs_input_float() {
            with_temp_home(|| {
                let mut app = App::new();
                let a = app.tasks.board.add("a").unwrap();
                let b = app.tasks.board.add("b").unwrap();
                app.tasks.board.assign(&a, "/tmp", "claude", "mux-a");
                app.tasks.board.assign(&b, "/tmp", "claude", "mux-b");
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
                app.tasks.board.set_priority(&a, TaskPriority::P1);
                assert_eq!(column_order(&app), vec![a, b]);
            });
        }

        #[test]
        fn toggle_done_completes_assigned_task_and_keeps_binding() {
            with_temp_home(|| {
                let mut app = App::new();
                let id = app.tasks.board.add("ship it").unwrap();
                app.tasks.board.assign(&id, "/tmp", "claude", "mux-dead");
                app.focus_task(&id);
                // No live mux session in the test env: the close is skipped
                // and the status stays the plain "done" line.
                assert_eq!(app.toggle_task_done().unwrap(), "done: ship it");
                let t = app.tasks.board.get(&id).unwrap();
                assert_eq!(t.status, TaskItemStatus::Done);
                // The binding survives completion so `f` can still resume.
                assert_eq!(t.tmux.as_deref(), Some("mux-dead"));
            });
        }
    }
}
