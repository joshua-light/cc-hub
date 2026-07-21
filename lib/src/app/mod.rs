use crate::agent::AgentConfig;
use crate::agent_runtime::{AgentRuntime, SystemAgentRuntime};
use crate::bookmarks::Bookmarks;
use crate::config;
use crate::conversation::StateExplanation;
use crate::folder_picker::{FolderPicker, PickerMode, Place};
use crate::live_view::LiveView;
use crate::metrics::{MetricsAnalysis, SelectableSession};
use crate::models::{ProjectGroup, SessionDetail, SessionInfo, SessionState, TaskBadge};
use crate::orchestrator::{TaskPriority, TaskState, TaskStatus};
use crate::projects_scan::ProjectsSnapshot;
use crate::session_count::SessionCounts;
use crate::tmux_pane::TmuxPaneView;
use crate::usage::UsageInfo;
use ratatui::text::Line;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod command;
mod metrics_view;
mod projects_view;
mod render_state;
mod sessions_view;
mod task_link_picker;
mod tasks_view;
mod todo_panel;

pub use command::{Command, Effect, GlobalCommand, SessionsCommand, TasksCommand};
pub use metrics_view::MetricsView;
pub use projects_view::ProjectsView;
pub use render_state::RenderState;
pub use sessions_view::{SessionsLayout, SessionsView};
pub use task_link_picker::{TaskLinkAction, TaskLinkChoice, TaskLinkPickerState, TaskLinkRow};
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

/// A task's display label: its Haiku title when present, else the first
/// line of the prompt truncated to fit picker rows and group headers.
fn task_display_title(task: &TaskState) -> String {
    task.title
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| crate::models::first_line_truncated(&task.prompt, 48))
}

/// Reorder one group's sessions so task-linked cards lead the group and
/// cards linked to the same task always sit next to each other. Clusters
/// order by the liveness bucket of their most-live member first — an idle
/// cluster trails an active one just like unlinked cards do — then by task
/// priority (P1 first; a task that's gone has no readable priority and
/// ranks last within its bucket), ties by their first member in the
/// incoming order, and the remaining members are pulled up behind that
/// anchor; unlinked cards follow in their incoming order. The pass adds no
/// churn of its own — links and priorities only change on user action, and
/// liveness flips already reorder the underlying sort.
fn cluster_by_task(
    sessions: Vec<SessionInfo>,
    links: &HashMap<String, crate::session_tasks::TaskLink>,
    priorities: &HashMap<String, TaskPriority>,
) -> Vec<SessionInfo> {
    let mut slots: Vec<Option<SessionInfo>> = sessions.into_iter().map(Some).collect();
    let mut clusters: Vec<((u8, u8), Vec<SessionInfo>)> = Vec::new();
    for i in 0..slots.len() {
        let Some(task_id) = slots[i]
            .as_ref()
            .and_then(|s| links.get(&s.session_id))
            .map(|l| l.task_id.clone())
        else {
            continue;
        };
        let mut members = vec![slots[i].take().expect("checked above")];
        for slot in slots.iter_mut().skip(i + 1) {
            let same_task = slot
                .as_ref()
                .and_then(|s| links.get(&s.session_id))
                .is_some_and(|l| l.task_id == task_id);
            if same_task {
                members.push(slot.take().expect("checked above"));
            }
        }
        let liveness = members
            .iter()
            .map(|s| s.state.liveness_rank())
            .min()
            .expect("cluster has at least its anchor");
        let rank = priorities.get(&task_id).map_or(u8::MAX, |p| *p as u8);
        clusters.push(((liveness, rank), members));
    }
    // Stable sort: equal-key clusters keep their incoming (liveness) order.
    clusters.sort_by_key(|(key, _)| *key);
    let mut out: Vec<SessionInfo> = clusters.into_iter().flat_map(|(_, m)| m).collect();
    out.extend(slots.into_iter().flatten());
    out
}

/// Task-link picker band order: the Tasks-board columns left to right
/// (To-Do → Planning → In Progress → Done). Personal-board tasks never hold
/// the orchestrated-only Review/Merging phases; they rank between In
/// Progress and Done for exhaustiveness.
fn task_link_status_rank(status: TaskStatus) -> u8 {
    match status {
        TaskStatus::Backlog => 0,
        TaskStatus::Planning => 1,
        TaskStatus::Running => 2,
        TaskStatus::Review => 3,
        TaskStatus::Merging => 4,
        TaskStatus::Done => 5,
    }
}

/// One task-link picker candidate plus its sort key parts:
/// `(status band, not-local-to-the-session's-cwd, updated_at)`. The detail
/// is just the status board label — every candidate is a personal-board
/// task, so a store/project prefix would repeat the same word on every row.
fn task_link_candidate(task: &TaskState, session_cwd: &str) -> (u8, bool, i64, TaskLinkChoice) {
    let title = task_display_title(task);
    // "Local" means the task's board assignment ran in the session's cwd —
    // those tasks lead their band.
    let local = task.cwd.as_deref() == Some(session_cwd);
    let choice = TaskLinkChoice {
        label: title.clone(),
        detail: task.status.board_label().to_string(),
        status: Some(task.status),
        action: TaskLinkAction::Link {
            task_id: task.task_id.clone(),
            project_id: task.project_id.clone(),
            title,
        },
    };
    (
        task_link_status_rank(task.status),
        !local,
        task.updated_at,
        choice,
    )
}

/// Short display name for an attachment in status messages: the URL itself
/// for URL artifacts, the *original* file's basename otherwise (the stored
/// copy's name carries a timestamp prefix nobody typed).
fn attachment_label(a: &crate::orchestrator::Artifact) -> String {
    if a.kind == "url" {
        return a.path.clone();
    }
    std::path::Path::new(&a.original)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| a.original.clone())
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
    /// Model list for `N` on the Sessions tab: pick which Claude model the
    /// new session starts with.
    ModelPicker,
    /// Fuzzy task selector for `L` on the Sessions tab: link (or unlink)
    /// the selected session to a personal-board or project task so the grid
    /// groups it under `project ▸ task`.
    TaskLinkPicker,
    GhCreateInput,
    ProjectsResult,
    Backlog,
    /// Scratch to-do side panel on the Sessions tab (toggled with `t`).
    TodoPanel,
    /// Centered single-line input for adding a task on the Tasks tab.
    TaskInput,
    /// Centered single-line input for editing the focused task's tags.
    TaskTags,
    /// Task Info popup for the focused board card: prompt + attachments,
    /// with per-attachment copy/open/remove.
    TaskInfo,
    /// Centered single-line input for attaching a file path or URL to the
    /// focused board card.
    TaskAttachInput,
    /// Typing edits the Tasks-board filter live; the board renders
    /// underneath, already narrowed. Enter keeps the filter, Esc clears it.
    TaskFilter,
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

/// Default model choices for the implicit Claude agent.
pub use crate::agent::DEFAULT_CLAUDE_MODELS as SPAWN_MODELS;

/// State behind [`View::ModelPicker`]: where the new session will spawn
/// (captured at open time so a rescan can't move the target), the live fuzzy
/// query, selected coding agent, and its filtered model choices.
#[derive(Clone, Debug)]
pub struct ModelPickerState {
    pub cwd: String,
    pub agent_id: String,
    pub selected: usize,
    pub filter: String,
    pub rows: Vec<ModelPickerRow>,
    pub choices: Vec<ModelPickerChoice>,
    agents: Vec<AgentConfig>,
}

/// One visible model-picker row plus the character indices highlighted in the
/// label or detail. Only the better-scoring side is highlighted.
#[derive(Clone, Debug, Default)]
pub struct ModelPickerRow {
    pub choice: usize,
    pub label_indices: Vec<usize>,
    pub detail_indices: Vec<usize>,
}

/// A model choice for the currently-selected coding agent. Agents with no
/// configured models get one choice with no override, leaving the provider
/// and model in their command untouched.
#[derive(Clone, Debug)]
pub struct ModelPickerChoice {
    pub label: String,
    pub detail: String,
    pub model_id: Option<String>,
}

impl ModelPickerState {
    pub(crate) fn new(cwd: String, default_agent_id: String, mut agents: Vec<AgentConfig>) -> Self {
        agents.sort_by(|a, b| a.id.cmp(&b.id));
        let agent_id = agents
            .iter()
            .find(|agent| agent.id == default_agent_id)
            .or_else(|| agents.first())
            .map(|agent| agent.id.clone())
            .unwrap_or(default_agent_id);
        let mut picker = Self {
            cwd,
            agent_id,
            selected: 0,
            filter: String::new(),
            rows: Vec::new(),
            choices: Vec::new(),
            agents,
        };
        picker.reload_choices();
        picker
    }

    pub fn push_filter(&mut self, c: char) {
        self.filter.push(c);
        self.refilter();
    }

    pub fn pop_filter(&mut self) {
        self.filter.pop();
        self.refilter();
    }

    pub fn move_selection(&mut self, delta: isize) {
        let last = self.rows.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    pub fn selected_model(&self) -> Option<(&str, Option<&str>)> {
        self.rows
            .get(self.selected)
            .and_then(|row| self.choices.get(row.choice))
            .map(|choice| (choice.label.as_str(), choice.model_id.as_deref()))
    }

    pub fn has_multiple_agents(&self) -> bool {
        self.agents.len() > 1
    }

    /// Tab: move to the next configured coding agent and rebuild the model
    /// choices appropriate for it. The old query is cleared because it was
    /// entered against a different candidate set.
    pub fn cycle_agent(&mut self) {
        if !self.has_multiple_agents() {
            return;
        }
        let current = self
            .agents
            .iter()
            .position(|agent| agent.id == self.agent_id)
            .unwrap_or(0);
        self.agent_id = self.agents[(current + 1) % self.agents.len()].id.clone();
        self.filter.clear();
        self.reload_choices();
    }

    fn reload_choices(&mut self) {
        self.choices = self
            .agents
            .iter()
            .find(|agent| agent.id == self.agent_id)
            .map(|agent| {
                if agent.models.is_empty() {
                    vec![ModelPickerChoice {
                        label: "Configured provider/model".into(),
                        detail: agent.command.clone(),
                        model_id: None,
                    }]
                } else {
                    agent
                        .models
                        .iter()
                        .map(|model| ModelPickerChoice {
                            label: model.label.clone(),
                            detail: model.id.clone(),
                            model_id: Some(model.id.clone()),
                        })
                        .collect()
                }
            })
            .unwrap_or_default();
        self.refilter();
    }

    fn refilter(&mut self) {
        if self.filter.is_empty() {
            self.rows = (0..self.choices.len())
                .map(|choice| ModelPickerRow {
                    choice,
                    ..Default::default()
                })
                .collect();
        } else {
            let mut scored = Vec::new();
            for (choice, model) in self.choices.iter().enumerate() {
                let label_match = crate::fuzzy::fuzzy_match(&self.filter, &model.label);
                let detail_match = crate::fuzzy::fuzzy_match(&self.filter, &model.detail);
                let label_score = label_match.as_ref().map(|m| m.score * 2);
                let detail_score = detail_match.as_ref().map(|m| m.score);
                let Some(score) = label_score.max(detail_score) else {
                    continue;
                };
                let row = if label_score >= detail_score {
                    ModelPickerRow {
                        choice,
                        label_indices: label_match.map(|m| m.indices).unwrap_or_default(),
                        detail_indices: Vec::new(),
                    }
                } else {
                    ModelPickerRow {
                        choice,
                        label_indices: Vec::new(),
                        detail_indices: detail_match.map(|m| m.indices).unwrap_or_default(),
                    }
                };
                scored.push((score, row));
            }
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.choice.cmp(&b.1.choice)));
            self.rows = scored.into_iter().map(|(_, row)| row).collect();
        }
        self.selected = 0;
    }
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
    runtime: Arc<dyn AgentRuntime>,
    pub sessions: SessionsView,
    pub projects: ProjectsView,
    pub metrics: MetricsView,
    pub tasks: TasksView,
    pub todo: TodoPanelState,
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
    pub state_debug: Option<(SessionInfo, StateExplanation)>,
    pub state_debug_lines: Vec<Line<'static>>,
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
    pub model_picker: Option<ModelPickerState>,
    /// State behind [`View::TaskLinkPicker`] (`L` on the Sessions tab).
    pub task_link_picker: Option<TaskLinkPickerState>,
    /// `session_id → TaskLink` sidecar snapshot driving the `project ▸ task`
    /// grouping in [`Self::build_groups`]. Reloaded from disk on every scan
    /// tick (mirroring how the scanner re-reads the title sidecar) so links
    /// written by another instance show up without a restart.
    pub(crate) session_task_links: HashMap<String, crate::session_tasks::TaskLink>,
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
    /// Watchdogs for user-initiated detached spawns ('n', folder picker).
    /// A spawn can succeed at the tmux level yet never start the agent —
    /// e.g. a shell-rc prompt blocking the detached pane — and without a
    /// watchdog that failure is invisible: the status bar said "started" and
    /// no card ever appears. Checked against each scan snapshot; see
    /// [`Self::check_spawn_watches`].
    spawn_watches: Vec<SpawnWatch>,
}

/// One pending spawn-verification: the agent must show up in a scan snapshot
/// hosted by `tmux_name` before `deadline`, else the user gets told. While
/// pending it also backs a placeholder "starting…" card in `cwd`'s group
/// (see [`App::build_groups`]), so the spawn is visible before the scanner
/// first sees the session.
struct SpawnWatch {
    tmux_name: String,
    /// Agent id: names the placeholder card and the diagnosis status line.
    agent: String,
    /// Where the session was spawned; parents the placeholder card's group.
    cwd: String,
    deadline: Instant,
}

/// How long a freshly spawned agent gets to appear in a scan snapshot before
/// its watch fires. Claude typically registers its session file within ~3s of
/// spawn; the slack covers slow cold starts. A false alarm costs one status
/// line, so generous beats jumpy.
const SPAWN_WATCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Synthetic session id backing a spawn watch's placeholder card. Prefixed so
/// it can never collide with a real scanner-issued session id, and derived
/// from the tmux name so the same spawn maps to the same id across rebuilds.
fn spawning_session_id(tmux_name: &str) -> String {
    format!("spawning:{}", tmux_name)
}

/// Loading-state stand-in for a spawn the scanner hasn't seen yet: Starting
/// (its own orbit spinner and color, and the idle liveness rank — the slot
/// the real card first appears in) with a "starting …" title. `tmux_session`
/// is real, so opening the card attaches to the pane where the agent is
/// actually booting.
fn spawning_placeholder(watch: &SpawnWatch) -> SessionInfo {
    let agent_kind = config::get()
        .agent(&watch.agent)
        .map(|a| a.kind)
        .unwrap_or(crate::agent::AgentKind::Claude);
    let mut info = SessionInfo {
        agent_id: watch.agent.clone(),
        agent_kind,
        pid: 0,
        session_id: spawning_session_id(&watch.tmux_name),
        cwd: watch.cwd.clone(),
        project_name: std::path::Path::new(&watch.cwd)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| watch.cwd.clone()),
        started_at: 0,
        last_activity: None,
        state: SessionState::Starting,
        last_user_message: None,
        summary: None,
        title: None,
        titling: false,
        model: None,
        git_branch: None,
        version: None,
        jsonl_path: None,
        tmux_session: Some(watch.tmux_name.clone()),
        current_tool: None,
        is_thinking: false,
        context_tokens: None,
        tool_uses_count: 0,
    };
    info.title = Some(format!("starting {}…", info.agent_badge()));
    info
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
            projects: ProjectsView::new(),
            metrics: MetricsView::new(),
            tasks: TasksView::new(),
            todo: TodoPanelState::new(),
            view: View::Grid,
            detail: None,
            detail_loading: false,
            render: RenderState::default(),
            should_quit: false,
            last_refresh: Instant::now(),
            live_view: None,
            status_msg: None,
            pending_confirm: None,
            state_debug: None,
            state_debug_lines: Vec::new(),
            usage: None,
            usage_line: Line::default(),
            session_counts: SessionCounts::default(),
            prompt_buffer: String::new(),
            rename_buffer: String::new(),
            rename_target: None,
            model_picker: None,
            task_link_picker: None,
            session_task_links: crate::session_tasks::load(),
            tmux_pane: None,
            folder_picker: None,
            bookmarks: Bookmarks::load(),
            gh_create_input: None,
            current_tab: Tab::Sessions,
            pending_dispatch: VecDeque::new(),
            last_dispatch_probe_at: None,
            image_picker: None,
            spawn_watches: Vec::new(),
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
        let (id, text) = (t.task_id.clone(), t.prompt.clone());
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
    /// Adds run through the quick syntax ([`crate::tasks::parse_quick_add`]):
    /// `#tag` and `!1`–`!4` tokens set tags/priority without extra
    /// round-trips through `t` and `1`–`4`. An input that is *only* syntax
    /// leaves no text, so nothing is added. Returns false when nothing
    /// changed.
    pub fn submit_task_input(&mut self) -> bool {
        let text = std::mem::take(&mut self.tasks.input);
        self.view = View::Grid;
        if let Some(id) = self.tasks.renaming.take() {
            return match self.tasks.board.rename(&id, &text) {
                Ok(changed) => changed,
                Err(e) => {
                    self.tasks.record_persistence_error("rename", e);
                    false
                }
            };
        }
        let quick = crate::tasks::parse_quick_add(&text);
        match self.tasks.board.add_configured(
            &quick.text,
            quick.tags,
            quick.priority.unwrap_or_default(),
        ) {
            Ok(Some(id)) => {
                self.focus_task(&id);
                true
            }
            Ok(None) => false,
            Err(e) => {
                self.tasks.record_persistence_error("add", e);
                false
            }
        }
    }

    /// `t` on a focused task: open the inline tag editor prefilled with the
    /// task's current tags (space-separated). Returns false when no task is
    /// focused.
    pub fn enter_task_tags(&mut self) -> bool {
        let Some(t) = self.selected_board_task() else {
            return false;
        };
        let (id, prefill) = (t.task_id.clone(), t.tags.join(" "));
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
        if let Err(e) = self
            .tasks
            .board
            .set_tags(&id, crate::tasks::parse_tags(&text))
        {
            self.tasks.record_persistence_error("tag update", e);
            return false;
        }
        self.focus_task(&id);
        true
    }

    /// `v` on a focused board card: open the Task Info popup (prompt +
    /// attachments). Returns false when no task is focused.
    pub fn enter_task_info(&mut self) -> bool {
        if self.selected_board_task().is_none() {
            return false;
        }
        self.tasks.info_sel = 0;
        self.render.task_info_scroll = 0;
        self.view = View::TaskInfo;
        true
    }

    pub fn close_task_info(&mut self) {
        self.tasks.info_sel = 0;
        self.render.task_info_scroll = 0;
        self.view = View::Grid;
    }

    /// The attachment under the Task Info popup cursor, if any. Used by the
    /// `c`/`o` keybinds to know what path to act on.
    pub fn selected_task_attachment(&self) -> Option<&crate::orchestrator::Artifact> {
        self.selected_board_task()?
            .artifacts
            .get(self.tasks.info_sel)
    }

    pub fn task_info_next(&mut self) {
        let n = self
            .selected_board_task()
            .map(|t| t.artifacts.len())
            .unwrap_or(0);
        if n == 0 {
            self.tasks.info_sel = 0;
            return;
        }
        self.tasks.info_sel = (self.tasks.info_sel + 1).min(n - 1);
    }

    pub fn task_info_prev(&mut self) {
        self.tasks.info_sel = self.tasks.info_sel.saturating_sub(1);
    }

    /// PgUp/PgDn handler for the Task Info popup. Negative steps scroll up;
    /// the renderer clamps the offset against content length.
    pub fn task_info_scroll_by(&mut self, delta: i32) {
        let cur = self.render.task_info_scroll as i32;
        self.render.task_info_scroll = (cur + delta).clamp(0, u16::MAX as i32) as u16;
    }

    /// `A` on a focused card (or `a` inside the Task Info popup): open the
    /// attach input. Returns false when no task is focused.
    pub fn enter_task_attach(&mut self, from_info: bool) -> bool {
        let Some(t) = self.selected_board_task() else {
            return false;
        };
        self.tasks.attaching = Some(t.task_id.clone());
        self.tasks.attach_from_info = from_info;
        self.tasks.input.clear();
        self.view = View::TaskAttachInput;
        true
    }

    pub fn close_task_attach(&mut self) {
        self.tasks.input.clear();
        self.tasks.attaching = None;
        self.view = if self.tasks.attach_from_info {
            View::TaskInfo
        } else {
            View::Grid
        };
        self.tasks.attach_from_info = false;
    }

    /// Commit the attach input: copy the file (or record the URL) into the
    /// task's own artifacts dir via the shared op and adopt the persisted
    /// state into the board. Empty input cancels. Returns false when nothing
    /// was attached (a failure lands in the persistence-error slot).
    pub fn submit_task_attach(&mut self) -> bool {
        let text = std::mem::take(&mut self.tasks.input);
        let raw = text.trim().to_string();
        let from_info = self.tasks.attach_from_info;
        self.tasks.attach_from_info = false;
        self.view = if from_info {
            View::TaskInfo
        } else {
            View::Grid
        };
        let Some(id) = self.tasks.attaching.take() else {
            return false;
        };
        if raw.is_empty() {
            return false;
        }
        match crate::ops::task::task_artifact_add(None, &id, &raw, None, None, false) {
            Ok(state) => {
                let label = state
                    .artifacts
                    .last()
                    .map(attachment_label)
                    .unwrap_or_default();
                let n = state.artifacts.len();
                self.tasks.board.adopt(state);
                if from_info {
                    // Land the popup cursor on what was just attached.
                    self.tasks.info_sel = n.saturating_sub(1);
                }
                self.set_status(format!("attached {}", label));
                true
            }
            Err(e) => {
                self.tasks.record_persistence_error("attach", e);
                false
            }
        }
    }

    /// `x` inside the Task Info popup: remove the selected attachment
    /// (record + stored copy — personal tasks only, the store op refuses
    /// anything else). Returns the status message, or `None` when no task is
    /// focused.
    pub fn remove_selected_attachment(&mut self) -> Option<String> {
        let t = self.selected_board_task()?;
        if t.artifacts.is_empty() {
            return Some("no attachments to remove".into());
        }
        let id = t.task_id.clone();
        let idx = self.tasks.info_sel.min(t.artifacts.len() - 1);
        match crate::ops::task::task_artifact_remove(&id, idx) {
            Ok((state, removed)) => {
                let remaining = state.artifacts.len();
                self.tasks.board.adopt(state);
                self.tasks.info_sel = self.tasks.info_sel.min(remaining.saturating_sub(1));
                Some(format!("removed {}", attachment_label(&removed)))
            }
            Err(e) => Some(format!("attachment remove failed: {}", e)),
        }
    }

    /// `p` on a focused card or inside the Task Info popup: attach `text`
    /// (read from the clipboard by the bin-side key arm, so tests can inject
    /// anything) as a `note` attachment — a fresh `.md` file in the task's
    /// artifacts dir. Returns the status message, or `None` when no task is
    /// focused.
    pub fn attach_text_to_selected(&mut self, text: &str) -> Option<String> {
        let t = self.selected_board_task()?;
        let id = t.task_id.clone();
        if text.trim().is_empty() {
            return Some("clipboard empty — nothing attached".into());
        }
        match crate::ops::task::task_artifact_add_text(None, &id, text) {
            Ok(state) => {
                let caption = state
                    .artifacts
                    .last()
                    .and_then(|a| a.caption.clone())
                    .unwrap_or_default();
                let n = state.artifacts.len();
                self.tasks.board.adopt(state);
                if self.view == View::TaskInfo {
                    // Land the popup cursor on what was just pasted.
                    self.tasks.info_sel = n.saturating_sub(1);
                }
                Some(format!("attached note — “{}”", caption))
            }
            Err(e) => Some(format!("attach failed: {}", e)),
        }
    }

    /// Route a bracketed-paste burst into whichever single-line task input is
    /// active (add/rename, tags, attach — they share one buffer). Newlines
    /// fold into spaces, other control characters are dropped. Returns false
    /// when the active view has no such input, so the caller ignores the
    /// paste like before.
    pub fn paste_into_input(&mut self, text: &str) -> bool {
        match self.view {
            View::TaskInput | View::TaskTags | View::TaskAttachInput => {}
            _ => return false,
        }
        for ch in text.chars() {
            if ch == '\n' || ch == '\r' {
                self.tasks.input.push(' ');
            } else if !ch.is_control() {
                self.tasks.input.push(ch);
            }
        }
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
    pub fn task_column(&self, status: TaskStatus) -> Vec<&TaskState> {
        let mut tasks = self.tasks.board.column(status);
        tasks.retain(|t| self.tasks.matches_filter(t));
        if matches!(status, TaskStatus::Planning | TaskStatus::Running) {
            let frozen = |id: &str| self.tasks.in_progress_order.iter().position(|x| x == id);
            // Stable sort: ids missing from the frozen order all key to MAX
            // and keep their relative insertion order at the tail.
            tasks.sort_by_key(|t| frozen(&t.task_id).unwrap_or(usize::MAX));
        }
        // Priority is the primary order in every column (P1 at the top). The
        // sort is stable, so equal-priority tasks keep the order established
        // above — insertion order, or the live columns' frozen needs-input
        // float.
        tasks.sort_by_key(|t| t.priority);
        tasks
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
        let statuses = column_statuses(col);
        if statuses.len() == 1 {
            return self.task_column(statuses[0]);
        }
        // Merged In Progress (absorbing Planning): both are live columns, so
        // apply the same frozen needs-input float then priority sort as
        // `task_column` does for a single live column.
        let mut tasks: Vec<&TaskState> = statuses
            .iter()
            .flat_map(|s| self.tasks.board.column(*s))
            .filter(|t| self.tasks.matches_filter(t))
            .collect();
        let frozen = |id: &str| self.tasks.in_progress_order.iter().position(|x| x == id);
        tasks.sort_by_key(|t| frozen(&t.task_id).unwrap_or(usize::MAX));
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

    /// Space on the board, status-aware: a Planning card tells its agent to
    /// proceed with the implementation; anything else toggles Done. Returns
    /// `None` when no task is focused.
    pub fn task_space_action(&mut self) -> Option<String> {
        match self.selected_board_task()?.status {
            TaskStatus::Planning => Some(self.proceed_selected_task()),
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
        let id = t.task_id.clone();
        let preview = crate::models::first_line_truncated(&t.prompt, 32);
        let live_tmux = t.tmux.clone().filter(|n| self.runtime.session_exists(n));
        if let Some(tmux) = live_tmux {
            return match self.runtime.send_prompt(&tmux, PROCEED_PROMPT) {
                Ok(()) => {
                    if let Err(e) = self.tasks.board.set_status(&id, TaskStatus::Running) {
                        return format!("proceed sent but task state write failed: {e}");
                    }
                    self.focus_task(&id);
                    format!(
                        "proceeding: {} — agent told to implement [{}]",
                        preview, tmux
                    )
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
        match self.runtime.spawn_session(
            &agent_id,
            &cwd,
            Some(crate::spawn::ResumeTarget::SessionId(sid)),
            None,
            None,
            false,
        ) {
            Ok(tmux) => {
                self.queue_pending_dispatch(tmux.clone(), PROCEED_PROMPT.to_string());
                if let Err(e) = self.tasks.board.rebind_tmux(&id, &tmux) {
                    let _ = self.runtime.kill_session(&tmux);
                    return format!("proceed cancelled: task binding write failed: {e}");
                }
                if let Err(e) = self.tasks.board.set_status(&id, TaskStatus::Running) {
                    return format!("agent resumed but task state write failed: {e}");
                }
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
        let id = t.task_id.clone();
        let preview = crate::models::first_line_truncated(&t.prompt, 32);
        let tmux = t.tmux.clone();
        if t.status == TaskStatus::Done {
            if let Err(e) = self.tasks.board.set_status(&id, TaskStatus::Backlog) {
                return Some(format!("reopen failed: {e}"));
            }
            self.tasks.clamp_row();
            return Some(format!("reopened: {}", preview));
        }
        Some(self.finish_task(&id, &preview, tmux.as_deref()))
    }

    /// Mark `id` Done and close its live agent session; the binding is kept
    /// so `f` on the Done card can still resume the transcript. Shared by
    /// Space (toggle) and the manual column move (`L` into Done).
    fn finish_task(&mut self, id: &str, preview: &str, tmux: Option<&str>) -> String {
        if let Err(e) = self.tasks.board.set_status(id, TaskStatus::Done) {
            return format!("finish failed: {e}");
        }
        self.tasks.clamp_row();
        let live = tmux.filter(|n| self.runtime.session_exists(n));
        match live {
            Some(name) => match self.runtime.kill_session(name) {
                Ok(()) => format!("done: {} — closed agent session [{}]", preview, name),
                Err(e) => format!(
                    "done: {} — closing agent session [{}] failed: {}",
                    preview, name, e
                ),
            },
            None => format!("done: {}", preview),
        }
    }

    /// `H`/`L`: move the focused card one column left (`dir < 0`) or right
    /// by hand. Planning is agent-owned — assignment is the only way in —
    /// so manual moves hop over it: To-Do ↔ In Progress ↔ Done. A Planning
    /// card can still be moved out: left parks it back in To-Do, right
    /// takes it to In Progress *without* telling the agent to proceed
    /// (Space stays the approve path). Moving into Done closes the live
    /// agent session exactly like Space; moving a Done card left reopens it
    /// into In Progress. The cursor rides with the card. Returns `None`
    /// when no task is focused or the move runs off the board's edge.
    pub fn move_selected_task(&mut self, dir: i8) -> Option<String> {
        let t = self.selected_board_task()?;
        let id = t.task_id.clone();
        let preview = crate::models::first_line_truncated(&t.prompt, 32);
        let tmux = t.tmux.clone();
        let to = match (t.status, dir < 0) {
            (TaskStatus::Backlog, false) => TaskStatus::Running,
            (TaskStatus::Planning, false) => TaskStatus::Running,
            (TaskStatus::Running, false) => TaskStatus::Done,
            (TaskStatus::Planning, true) => TaskStatus::Backlog,
            (TaskStatus::Running, true) => TaskStatus::Backlog,
            (TaskStatus::Done, true) => TaskStatus::Running,
            (TaskStatus::Backlog, true) | (TaskStatus::Done, false) => return None,
            // Orchestrated-only states never appear on the personal board.
            (TaskStatus::Review | TaskStatus::Merging, _) => return None,
        };
        if to == TaskStatus::Done {
            let msg = self.finish_task(&id, &preview, tmux.as_deref());
            self.focus_task(&id);
            return Some(msg);
        }
        let from_planning = t.status == TaskStatus::Planning;
        if let Err(e) = self.tasks.board.set_status(&id, to) {
            return Some(format!("move failed: {e}"));
        }
        self.focus_task(&id);
        self.tasks.clamp_row();
        let label = match to {
            TaskStatus::Backlog => "To-Do",
            TaskStatus::Running => "In Progress",
            _ => unreachable!("manual moves only land in To-Do/In Progress here"),
        };
        Some(if from_planning && to == TaskStatus::Running {
            format!(
                "moved: {} → {} — agent not told to proceed (Space does that)",
                preview, label
            )
        } else {
            format!("moved: {} → {}", preview, label)
        })
    }

    /// `1`–`4` on a focused task: set its priority. Priority is the column's
    /// primary sort key, so the card may jump to a new row; the cursor rides
    /// with it (resolved by id through [`Self::focus_task`]). Returns a status
    /// line, or `None` when no task is focused.
    pub fn set_selected_task_priority(&mut self, priority: TaskPriority) -> Option<String> {
        let t = self.selected_board_task()?;
        let id = t.task_id.clone();
        let preview = crate::models::first_line_truncated(&t.prompt, 32);
        if let Err(e) = self.tasks.board.set_priority(&id, priority) {
            return Some(format!("priority update failed: {e}"));
        }
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
                recents.push((
                    t.created_at.max(t.done_at.unwrap_or(0)).max(0) as u64,
                    PathBuf::from(cwd),
                ));
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
        if t.status == TaskStatus::Done {
            return false;
        }
        let id = t.task_id.clone();
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
        if t.status == TaskStatus::Done {
            return None;
        }
        let id = t.task_id.clone();
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
        let prompt = planning_prompt(&task.prompt);
        let agent_id = config::get().default_session_agent_id();
        let supports_initial_prompt = config::get()
            .agent(&agent_id)
            .is_some_and(|a| a.supports_initial_prompt());
        match self.runtime.spawn_session(
            &agent_id,
            cwd,
            None,
            supports_initial_prompt.then_some(prompt.as_str()),
            None,
            false,
        ) {
            Ok(tmux) => {
                if !supports_initial_prompt {
                    self.queue_pending_dispatch(tmux.clone(), prompt);
                }
                if let Err(e) = self.tasks.board.assign(&id, cwd, &agent_id, &tmux) {
                    let cleanup = self
                        .runtime
                        .kill_session(&tmux)
                        .err()
                        .map(|cleanup| format!("; cleanup failed: {cleanup}"))
                        .unwrap_or_default();
                    return format!("assign rolled back: task write failed: {e}{cleanup}");
                }
                self.focus_task(&id);
                format!(
                    "assigned {} [{}] — planning (Space approves the plan)",
                    agent_id, tmux
                )
            }
            Err(e) => format!("assign failed: {}", e),
        }
    }

    /// Point a task's binding at a freshly-spawned mux session (resume path).
    pub fn task_session_is_live(&self, tmux: &str) -> bool {
        self.runtime.session_exists(tmux)
    }

    pub fn resume_board_task(&mut self, task: &TaskState) -> Result<String, String> {
        let sid = task
            .session_id
            .clone()
            .ok_or_else(|| "task has no resumable session id".to_string())?;
        let cwd = task
            .cwd
            .as_deref()
            .ok_or_else(|| "task has no assignment directory".to_string())?;
        let agent_id = task.agent_id.as_deref().unwrap_or("claude");
        let tmux = self
            .runtime
            .spawn_session(
                agent_id,
                cwd,
                Some(crate::spawn::ResumeTarget::SessionId(sid)),
                None,
                None,
                false,
            )
            .map_err(|e| format!("resume failed: {e}"))?;
        if let Err(e) = self.tasks.board.rebind_tmux(&task.task_id, &tmux) {
            let _ = self.runtime.kill_session(&tmux);
            return Err(format!("resume rolled back: task write failed: {e}"));
        }
        Ok(tmux)
    }

    /// Delete the focused task. The bound agent session (if any) is left
    /// running — it still shows on the Sessions tab. The task lands in the
    /// undo slot (`u`) and the on-disk archive. Returns the status line.
    pub fn delete_selected_task(&mut self) -> Option<String> {
        let id = self.selected_board_task()?.task_id.clone();
        let removed = match self.tasks.board.remove(&id) {
            Ok(removed) => removed?,
            Err(e) => return Some(format!("delete failed: {e}")),
        };
        self.tasks.clamp_row();
        let preview = crate::models::first_line_truncated(&removed.prompt, 32);
        let tmux = removed.tmux.clone();
        self.tasks.undo = Some(vec![removed]);
        Some(match tmux {
            Some(tmux) => format!(
                "deleted: {} (u undoes) — agent session [{}] left running (close it from Sessions)",
                preview, tmux
            ),
            None => format!("deleted: {} (u undoes)", preview),
        })
    }

    pub fn clear_done_tasks(&mut self) {
        let removed = match self.tasks.board.clear_done() {
            Ok(removed) => removed,
            Err(e) => {
                self.set_status(format!("clear done failed: {e}"));
                return;
            }
        };
        self.tasks.clamp_row();
        if removed.is_empty() {
            self.set_status("no done tasks to clear".to_string());
        } else {
            self.set_status(format!(
                "cleared {} done task{} (u undoes)",
                removed.len(),
                if removed.len() == 1 { "" } else { "s" }
            ));
            self.tasks.undo = Some(removed);
        }
    }

    /// `u`: restore the last `x`/`c` removal (one batch deep). Tasks whose
    /// id somehow returned to the board (hand edit) are skipped. The cursor
    /// jumps to the first restored card. Returns `None` when there is
    /// nothing to undo.
    pub fn undo_task_delete(&mut self) -> Option<String> {
        let batch = self.tasks.undo.take()?;
        let mut first: Option<String> = None;
        let mut restored = 0usize;
        for item in batch {
            let id = item.task_id.clone();
            match self.tasks.board.restore(item) {
                Ok(true) => {
                    restored += 1;
                    first.get_or_insert(id);
                }
                Ok(false) => {}
                Err(e) => {
                    self.set_status(format!("undo failed: {e}"));
                    return None;
                }
            }
        }
        if restored == 0 {
            return Some("nothing to restore — the tasks are already back".into());
        }
        if let Some(id) = first {
            self.focus_task(&id);
        }
        Some(format!(
            "restored {} task{}",
            restored,
            if restored == 1 { "" } else { "s" }
        ))
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

    pub fn toggle_show_inactive(&mut self) {
        self.sessions.show_inactive = !self.sessions.show_inactive;
        self.rebuild_groups();
    }

    pub fn toggle_show_orch_workers(&mut self) {
        self.sessions.show_orch_workers = !self.sessions.show_orch_workers;
        self.rebuild_groups();
    }

    pub fn toggle_sessions_layout(&mut self) {
        self.sessions.layout = match self.sessions.layout {
            SessionsLayout::Grid => SessionsLayout::List,
            SessionsLayout::List => SessionsLayout::Grid,
        };
        // The two layouts measure content height differently, so a
        // carried-over offset can strand the viewport past the content;
        // the renderer re-clamps onto the selection from zero next frame.
        self.render.grid_scroll = 0;
    }

    /// Column count the Sessions nav methods should move by: the derived
    /// grid column count, or 1 in the linear list layout.
    pub(crate) fn sessions_nav_cols(&self) -> u16 {
        match self.sessions.layout {
            SessionsLayout::Grid => self.render.grid_cols,
            SessionsLayout::List => 1,
        }
    }

    pub fn set_tab(&mut self, tab: Tab) {
        // Entering the Tasks tab re-reads the board so edits from another
        // instance (or a hand-edited tasks.json) show up, mirroring the
        // to-do panel's reload-on-open. The reload and the re-floated
        // In Progress order can both rearrange rows, so the cursor is
        // re-anchored to the task it was on (by id) rather than left at a
        // stale (col, row) pointing at whatever card landed there.
        if tab == Tab::Tasks && self.current_tab != Tab::Tasks {
            let keep = self.selected_board_task().map(|t| t.task_id.clone());
            self.tasks.reload();
            self.refresh_in_progress_order();
            if let Some(id) = keep {
                self.focus_task(&id);
            }
            // No-op after a successful re-focus; catches the id having been
            // deleted out from under us (and the no-selection case).
            self.tasks.clamp_row();
            if let Some(error) = self.tasks.take_persistence_error() {
                self.set_status(error);
            }
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
        // Review tasks are orchestrated by construction.
        let Some(project_id) = t.project_id.clone() else {
            return ApproveOutcome::NotReviewTask;
        };
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

    fn metrics_scroll_down(&mut self) {
        self.render.metrics_scroll = self.render.metrics_scroll.saturating_add(3);
    }

    fn metrics_scroll_up(&mut self) {
        self.render.metrics_scroll = self.render.metrics_scroll.saturating_sub(3);
    }

    /// Down/`j` on the Metrics tab. With a row selected, advance the cursor
    /// (the renderer keeps it on screen). With nothing selected, engage the
    /// first session row already visible — so selection only kicks in once the
    /// lists scroll into view — otherwise keep free-scrolling toward them.
    /// Reads the viewport geometry the renderer synced into [`RenderState`].
    pub fn metrics_nav_down(&mut self) {
        match self.metrics.selected {
            Some(i) if !self.metrics.rows.is_empty() => {
                self.metrics.selected = Some((i + 1).min(self.metrics.rows.len() - 1));
            }
            _ => match self.first_visible_metrics_row() {
                Some(idx) => self.metrics.selected = Some(idx),
                None => self.metrics_scroll_down(),
            },
        }
    }

    /// Up/`k` on the Metrics tab. Walk the cursor back up; pressing up past the
    /// first session row releases the selection so free-scrolling (and reaching
    /// the Overview at the very top) resumes.
    pub fn metrics_nav_up(&mut self) {
        match self.metrics.selected {
            Some(0) => self.metrics.selected = None,
            Some(i) => self.metrics.selected = Some(i - 1),
            None => self.metrics_scroll_up(),
        }
    }

    /// Index (into `MetricsView::rows`) of the first selectable session row
    /// currently inside the viewport, using the offsets/height the renderer
    /// last synced into [`RenderState`]. `None` when no session row is on
    /// screen.
    fn first_visible_metrics_row(&self) -> Option<usize> {
        let h = self.render.metrics_view_height;
        if h == 0 {
            return None;
        }
        let top = self.render.metrics_scroll;
        self.render
            .metrics_row_lines
            .iter()
            .position(|&l| (l as u16) >= top && (l as u16) < top.saturating_add(h))
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

    /// `p` on the Sessions tab: the same places picker the task-assign
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

    /// `N` on the Sessions tab: open the model picker for a new session in
    /// the selected session's cwd (falling back to `$HOME`). The target cwd
    /// and agent are captured now so a rescan can't move them under the
    /// popup; the spawn happens in [`Self::spawn_from_model_picker`].
    pub fn enter_model_picker(&mut self) {
        let Some(cwd) = self.default_spawn_cwd() else {
            self.set_status("no cwd to spawn in".into());
            return;
        };
        self.model_picker = Some(ModelPickerState::new(
            cwd,
            config::get().default_session_agent_id(),
            config::get().resolved_agents().into_values().collect(),
        ));
        self.view = View::ModelPicker;
    }

    pub fn close_model_picker(&mut self) {
        self.model_picker = None;
        self.view = View::Grid;
    }

    /// Move the model-picker highlight by `delta` rows, clamped to the live
    /// filtered result list.
    pub fn model_picker_move(&mut self, delta: isize) {
        if let Some(picker) = self.model_picker.as_mut() {
            picker.move_selection(delta);
        }
    }

    pub fn cycle_model_picker_agent(&mut self) {
        if let Some(picker) = self.model_picker.as_mut() {
            picker.cycle_agent();
        }
    }

    /// Enter on the model picker: spawn a fresh session in the captured cwd,
    /// applying the highlighted model override when that agent supports one,
    /// with the spawn watchdog armed (same contract as `n`).
    pub fn spawn_from_model_picker(&mut self) {
        let Some((label, model_id)) = self
            .model_picker
            .as_ref()
            .and_then(ModelPickerState::selected_model)
            .map(|(label, model_id)| (label.to_string(), model_id.map(str::to_string)))
        else {
            return;
        };
        let Some(picker) = self.model_picker.take() else {
            return;
        };
        self.view = View::Grid;
        let status = match self.runtime.spawn_session(
            &picker.agent_id,
            &picker.cwd,
            None,
            None,
            model_id.as_deref(),
            false,
        ) {
            Ok(name) => {
                let status = format!("started {} ({}) [{}]", picker.agent_id, label, name);
                self.watch_spawn(name, picker.agent_id, picker.cwd);
                status
            }
            Err(e) => format!("spawn failed: {}", e),
        };
        self.set_status(status);
    }

    /// `L` on the Sessions tab: open the fuzzy task selector to link the
    /// selected session to a task (or unlink it). The target session is
    /// captured now so a rescan can't move it under the popup. Returns
    /// `false` when nothing is selected or there is nothing to offer.
    pub fn enter_task_link_picker(&mut self) -> bool {
        let Some(session) = self.selected_session_info().cloned() else {
            return false;
        };
        let current = self.session_task_links.get(&session.session_id).cloned();
        let mut choices: Vec<TaskLinkChoice> = Vec::new();
        if current.is_some() {
            choices.push(TaskLinkChoice {
                label: "✕ unlink".into(),
                detail: "remove the task link".into(),
                status: None,
                action: TaskLinkAction::Unlink,
            });
        }
        choices.extend(self.task_link_candidates(&session));
        if choices.is_empty() {
            return false;
        }
        let label = session
            .title
            .clone()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| crate::models::short_sid(&session.session_id).to_string());
        self.task_link_picker = Some(TaskLinkPickerState::new(
            session.session_id.clone(),
            label,
            choices,
            current.as_ref().map(|l| l.task_id.as_str()),
        ));
        self.view = View::TaskLinkPicker;
        true
    }

    /// Every linkable task — the personal board, i.e. exactly what the
    /// Tasks tab shows (orchestrated project tasks are deliberately absent:
    /// they live behind the WIP-gated Projects tab and would read as noise
    /// from nowhere) — as picker rows, banded by status in Tasks-board
    /// column order (To-Do → Planning → In Progress → Done); within a band,
    /// tasks local to the session's cwd first, then newest first.
    fn task_link_candidates(&self, session: &SessionInfo) -> Vec<TaskLinkChoice> {
        let mut candidates: Vec<(u8, bool, i64, TaskLinkChoice)> = self
            .tasks
            .board
            .tasks()
            .iter()
            .map(|t| task_link_candidate(t, &session.cwd))
            .collect();
        candidates.sort_by_key(|(band, not_local, updated_at, choice)| {
            (
                *band,
                *not_local,
                std::cmp::Reverse(*updated_at),
                choice.label.to_lowercase(),
            )
        });
        candidates.into_iter().map(|(_, _, _, c)| c).collect()
    }

    pub fn close_task_link_picker(&mut self) {
        self.task_link_picker = None;
        self.view = View::Grid;
    }

    /// Move the task-link-picker highlight by `delta` rows, clamped to the
    /// live filtered result list.
    pub fn task_link_picker_move(&mut self, delta: isize) {
        if let Some(picker) = self.task_link_picker.as_mut() {
            picker.move_selection(delta);
        }
    }

    /// Enter on the task-link picker: persist the link (or unlink) for the
    /// captured session and regroup the grid immediately. An empty match
    /// list keeps the picker open, mirroring the model picker.
    pub fn confirm_task_link_picker(&mut self) {
        let Some(action) = self
            .task_link_picker
            .as_ref()
            .and_then(TaskLinkPickerState::selected_action)
            .cloned()
        else {
            return;
        };
        let Some(picker) = self.task_link_picker.take() else {
            return;
        };
        self.view = View::Grid;
        let sid = picker.session_id;
        let status = match action {
            TaskLinkAction::Unlink => match crate::session_tasks::unlink(&sid) {
                Ok(()) => {
                    self.session_task_links.remove(&sid);
                    "task link removed".to_string()
                }
                Err(e) => {
                    log::warn!("task link: unlink failed for {}: {}", sid, e);
                    format!("unlink failed: {}", e)
                }
            },
            TaskLinkAction::Link {
                task_id,
                project_id,
                title,
            } => {
                let link = crate::session_tasks::TaskLink {
                    task_id,
                    project_id,
                    title: title.clone(),
                };
                match crate::session_tasks::link(&sid, link.clone()) {
                    Ok(()) => {
                        self.session_task_links.insert(sid.clone(), link);
                        format!("linked to “{}”", title)
                    }
                    Err(e) => {
                        log::warn!("task link: persist failed for {}: {}", sid, e);
                        format!("link failed: {}", e)
                    }
                }
            }
        };
        // No group rebuild: links don't shape the grid's groups — the card
        // badge resolves live from `session_task_links` on the next draw.
        self.set_status(status);
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

    pub fn close_prompt_input(&mut self) {
        self.prompt_buffer.clear();
        self.projects.pending_cwd = None;
        self.projects.pending_agent_id = None;
        self.projects.creating_task = false;
        self.view = View::Grid;
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
        // Keep the snapshot rebuild_groups derives from in sync, so a rebuild
        // before the next scan doesn't revert to the old title (same hazard as
        // ack_selected). The next scan restores it from the title cache.
        for session in &mut self.sessions.last_sessions {
            if session.session_id == sid {
                session.title = Some(title.clone());
                session.titling = false;
            }
        }
        Some((sid, title))
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
                self.runtime.ready_for_input(&pd.tmux)
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
        // Apply the downgrade to the snapshot rebuild_groups derives from, so
        // a rebuild before the next scan (filter toggle, projects snapshot)
        // doesn't revert the ack. The next scan re-derives it via is_acked.
        if let Some(s) = self
            .sessions
            .last_sessions
            .iter_mut()
            .find(|s| s.session_id == id)
        {
            s.state = SessionState::Idle;
        }
        // Rebuild immediately: the badge flips to idle and the card re-slots
        // into the idle bucket without waiting for the next scan tick.
        // adopt_groups re-anchors the cursor on the acked session's id, so
        // the selection follows the card to its new slot.
        self.rebuild_groups();
        true
    }

    pub fn scroll_down(&mut self) {
        self.render.popup_scroll = self.render.popup_scroll.saturating_add(3);
    }

    pub fn scroll_up(&mut self) {
        self.render.popup_scroll = self.render.popup_scroll.saturating_sub(3);
    }

    pub fn enter_popup(&mut self) {
        self.view = View::Popup;
        self.detail_loading = true;
        self.render.popup_scroll = 0;
    }

    pub fn close_popup(&mut self) {
        self.view = View::Grid;
        self.detail = None;
        self.detail_loading = false;
        self.render.popup_scroll = 0;
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
        self.render.state_debug_scroll = 0;
    }

    pub fn close_state_debug(&mut self) {
        self.view = View::Grid;
        self.state_debug = None;
        self.state_debug_lines.clear();
        self.render.state_debug_scroll = 0;
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
        self.render.state_debug_scroll = self.render.state_debug_scroll.saturating_add(3);
    }

    pub fn debug_scroll_up(&mut self) {
        self.render.state_debug_scroll = self.render.state_debug_scroll.saturating_sub(3);
    }

    pub fn selected_session_id(&self) -> Option<String> {
        self.sessions.selected_session_id()
    }

    pub fn selected_session_info(&self) -> Option<&SessionInfo> {
        self.sessions.selected_session_info()
    }

    /// Register a watchdog for a just-spawned detached agent session and
    /// surface its placeholder card immediately, cursor on it — the spawned
    /// agent takes seconds to write a session file the scanner can see, and
    /// until this rebuild the keypress had no visible effect.
    pub fn watch_spawn(&mut self, tmux_name: String, agent: String, cwd: String) {
        let placeholder_id = spawning_session_id(&tmux_name);
        self.spawn_watches.push(SpawnWatch {
            tmux_name,
            agent,
            cwd,
            deadline: Instant::now() + SPAWN_WATCH_TIMEOUT,
        });
        self.rebuild_groups();
        self.select_session_by_id(&placeholder_id);
    }

    /// Move the cursor onto `session_id` if it's currently visible.
    fn select_session_by_id(&mut self, session_id: &str) {
        let found = self
            .sessions
            .groups
            .iter()
            .enumerate()
            .find_map(|(gi, group)| {
                group
                    .sessions
                    .iter()
                    .position(|s| s.session_id == session_id)
                    .map(|si| (gi, si))
            });
        if let Some((gi, si)) = found {
            self.sessions.sel_group = gi;
            self.sessions.sel_in_group = si;
        }
    }

    /// Resolve pending spawn watches against a scan snapshot. A watch clears
    /// when any session is hosted by its tmux name; one that expires first
    /// means the agent never came up (rc prompt, instant crash) — surface a
    /// diagnosis instead of leaving the silent "started" status as the last
    /// word. Returns true when a diagnosis was set, so the caller repaints
    /// even on an otherwise no-change tick.
    fn check_spawn_watches(&mut self, sessions: &[SessionInfo], now: Instant) -> bool {
        if self.spawn_watches.is_empty() {
            return false;
        }
        self.spawn_watches.retain(|w| {
            !sessions
                .iter()
                .any(|s| s.tmux_session.as_deref() == Some(w.tmux_name.as_str()))
        });
        let mut fired = false;
        let mut i = 0;
        while i < self.spawn_watches.len() {
            if self.spawn_watches[i].deadline <= now {
                let w = self.spawn_watches.remove(i);
                let msg = crate::spawn::diagnose_stalled_spawn(&w.tmux_name, &w.agent);
                log::warn!("spawn watch: {}", msg);
                self.set_status(msg);
                fired = true;
            } else {
                i += 1;
            }
        }
        fired
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
        // Refresh the session→task sidecar so links written by another
        // instance (or the CLI) regroup the grid without a restart — the
        // same per-tick re-read the scanner does for the title sidecar.
        self.session_task_links = crate::session_tasks::load();
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

        let spawn_watch_fired = self.check_spawn_watches(&sessions, Instant::now());

        // Resolve task-board agent bindings: a freshly-assigned task knows
        // only its tmux name until the scanner sees the session; learning the
        // session id here is what lets `f` resume after the tmux dies.
        let task_bindings_changed = match self.tasks.board.bind_sessions(&sessions) {
            Ok(changed) => changed,
            Err(e) => {
                let message = format!("task session binding failed: {e}");
                self.tasks.persistence_error = Some(message.clone());
                if self.current_tab == Tab::Tasks {
                    self.set_status(message);
                }
                false
            }
        };

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
            return changed || task_bindings_changed || spawn_watch_fired;
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

        // Placeholder cards for spawns the scanner hasn't seen yet. Checked
        // against the unfiltered snapshot: once any session is hosted by the
        // watched tmux name, the real card replaces the placeholder (the
        // watch itself is cleared by `check_spawn_watches` on the same tick).
        let placeholders: Vec<SessionInfo> = self
            .spawn_watches
            .iter()
            .filter(|w| {
                !sessions
                    .iter()
                    .any(|s| s.tmux_session.as_deref() == Some(w.tmux_name.as_str()))
            })
            .map(spawning_placeholder)
            .collect();

        let mut sessions: Vec<SessionInfo> = sessions
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

        // Placeholders ride the same sort/cluster pipeline as scanned
        // sessions so each one occupies the exact slot its real card will
        // take over. Prepended: the real session will be the group's newest,
        // and the scanner sorts newest-first within a liveness bucket.
        sessions.splice(0..0, placeholders);

        // Re-assert the liveness bucketing (active first, idle after,
        // inactive last) on the app-side state. The scanner already sorts by
        // (bucket, started_at desc, session id), but acks downgrade sessions
        // to Idle *after* that sort, so the scan order can be stale for acked
        // cards. The sort is stable and keys on the bucket alone: within a
        // bucket the scanner's order is preserved, and active-state flavors
        // (processing vs waiting) still can't reshuffle cards under the
        // cursor.
        sessions.sort_by_key(|s| s.state.liveness_rank());

        // Group sessions by cwd. HashMap::entry preserves bucket-relative
        // order, so each group comes out in the flat list's order.
        let mut group_map: HashMap<String, Vec<SessionInfo>> = HashMap::new();
        for s in sessions {
            group_map.entry(s.cwd.clone()).or_default().push(s);
        }

        // Cluster ordering needs each linked task's priority; resolve them
        // once per rebuild instead of per group. Gone tasks simply stay out
        // of the map and rank last.
        let priorities: HashMap<String, TaskPriority> = self
            .session_task_links
            .values()
            .filter_map(|l| Some((l.task_id.clone(), self.task_priority(&l.task_id)?)))
            .collect();

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
                    sessions: cluster_by_task(sessions, &self.session_task_links, &priorities),
                }
            })
            .collect();

        // Sort groups by lowercased name, tie-broken by cwd. `name` is only
        // the cwd basename, so distinct projects that share a basename
        // (~/work/api vs ~/personal/api) tie; without the cwd tie-break the
        // stable sort would preserve HashMap's random per-instance iteration
        // order and those groups would swap slots between scan ticks.
        groups.sort_by_key(|a| (a.name.to_lowercase(), a.cwd.clone()));
        groups
    }

    /// Priority of a task while it's still readable — the personal board
    /// first, then the projects snapshot (the same resolution order as
    /// [`Self::task_badge`]). `None` once the task is gone.
    fn task_priority(&self, task_id: &str) -> Option<TaskPriority> {
        self.tasks
            .board
            .get(task_id)
            .map(|t| t.priority)
            .or_else(|| {
                self.projects
                    .snapshot
                    .tasks
                    .values()
                    .flatten()
                    .find(|t| t.task_id == task_id)
                    .map(|t| t.priority)
            })
    }

    /// Resolve a session's task link (`L`) to its card badge: live title and
    /// status from the personal board or the projects snapshot while the
    /// task is still readable, else the sidecar's title snapshot. `stale`
    /// covers both a missing task and a Done one — either way the badge
    /// dims so the link visibly outlived its task. `None` for unlinked
    /// sessions.
    pub(crate) fn task_badge(&self, session_id: &str) -> Option<TaskBadge> {
        let link = self.session_task_links.get(session_id)?;
        let task_id = link.task_id.as_str();
        let live = self
            .tasks
            .board
            .get(task_id)
            .map(|t| (task_display_title(t), t.status, t.priority))
            .or_else(|| {
                self.projects
                    .snapshot
                    .tasks
                    .values()
                    .flatten()
                    .find(|t| t.task_id == task_id)
                    .map(|t| (task_display_title(t), t.status, t.priority))
            });
        Some(match live {
            Some((title, status, priority)) => TaskBadge {
                task_id: task_id.to_string(),
                title,
                priority: Some(priority),
                stale: status == TaskStatus::Done,
            },
            None => {
                let snapshot = Some(link.title.clone())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| crate::orchestrator::short_task_id(task_id));
                TaskBadge {
                    task_id: task_id.to_string(),
                    title: snapshot,
                    priority: None,
                    stale: true,
                }
            }
        })
    }

    /// Install a freshly-built group list and re-anchor the selection on the
    /// session id that was selected before, clamping when it's gone.
    fn adopt_groups(&mut self, groups: Vec<ProjectGroup>) {
        let prev_id = self.selected_session_id();
        let sel_before = (self.sessions.sel_group, self.sessions.sel_in_group);
        self.sessions.groups = groups;

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
        // A rebuild (filter toggle, or a projects snapshot reclassifying tmux
        // names) can reveal already-seen sessions without a scan. Register
        // them as known now so the next genuine membership change doesn't
        // mistake one for a fresh arrival and teleport the cursor onto it.
        self.sync_known_session_ids();
    }

    /// Refresh `known_session_ids` to the currently-visible session ids, but
    /// only once the first scan has seeded it. Rebuilds that happen outside a
    /// scan call this so a later scan's focus-jump fires only for genuinely
    /// new sessions — never for one a rebuild merely made visible. Skipping
    /// the `None` case preserves `update_sessions`' skip-jump-on-first-load.
    fn sync_known_session_ids(&mut self) {
        if self.sessions.known_session_ids.is_none() {
            return;
        }
        let current: HashSet<String> = self
            .sessions
            .groups
            .iter()
            .flat_map(|g| g.sessions.iter().map(|s| s.session_id.clone()))
            .collect();
        self.sessions.known_session_ids = Some(current);
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
        self.sessions.attention_count()
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

    /// Open the Projects "Result" popup for the currently-selected task.
    /// Returns false when no task is selected, so the caller can surface a
    /// status-bar hint instead of opening an empty popup.
    pub fn enter_projects_result(&mut self) -> bool {
        if self.selected_project_task().is_none() {
            return false;
        }
        self.projects.result_artifact_sel = 0;
        self.render.result_scroll = 0;
        self.render.result_artifact_expanded = false;
        self.view = View::ProjectsResult;
        true
    }

    pub fn close_projects_result(&mut self) {
        self.view = View::Grid;
        self.projects.result_artifact_sel = 0;
        self.render.result_scroll = 0;
        self.render.result_artifact_expanded = false;
    }

    /// The artifact under the popup cursor, if any. Used by the `c` and `o`
    /// keybinds to know what path to act on.
    pub fn selected_result_artifact(&self) -> Option<&crate::orchestrator::Artifact> {
        self.projects.selected_result_artifact()
    }

    /// PgUp/PgDn handler for the Result popup. Negative steps scroll up; the
    /// renderer clamps the offset against content length so we never scroll
    /// past the end. The scroll offset lives in [`RenderState`].
    pub fn result_scroll_by(&mut self, delta: i32) {
        let cur = self.render.result_scroll as i32;
        let next = (cur + delta).max(0);
        self.render.result_scroll = next.min(u16::MAX as i32) as u16;
    }

    pub fn toggle_result_artifact_expanded(&mut self) {
        if self.projects.selected_result_artifact().is_none() {
            return;
        }
        self.render.result_artifact_expanded = !self.render.result_artifact_expanded;
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
    use crate::agent_runtime::testing::RecordingRuntime;
    use crate::orchestrator::{Project, TaskState, TaskStatus, Worker};
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    #[cfg(unix)]
    fn task_controller_uses_runtime_boundary_to_proceed() {
        crate::test_util::with_temp_home(|| {
            let runtime = Arc::new(RecordingRuntime::default());
            let mut app = App::new_with_runtime(runtime.clone());
            let id = app.tasks.board.add("implement it").unwrap().unwrap();
            app.tasks
                .board
                .assign(&id, "/tmp", "claude", "mux-task")
                .unwrap();
            app.focus_task(&id);

            let message = app.proceed_selected_task();

            assert!(message.contains("agent told to implement"));
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Running
            );
            assert_eq!(
                runtime.prompts.lock().unwrap().as_slice(),
                &[("mux-task".into(), PROCEED_PROMPT.into())]
            );
        });
    }

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

    // A watch clears as soon as any live session maps to its tmux name —
    // even on the same tick its deadline passes (appearance wins over expiry).
    #[test]
    fn spawn_watch_clears_when_agent_appears() {
        let mut app = App::new();
        app.watch_spawn("cchub-w-1".into(), "claude".into(), "/tmp".into());
        let sessions = vec![fake_session("cchub-w-1", SessionState::Idle)];
        let fired = app.check_spawn_watches(&sessions, Instant::now() + 2 * SPAWN_WATCH_TIMEOUT);
        assert!(!fired);
        assert!(app.spawn_watches.is_empty());
        assert!(app.status_msg.is_none());
    }

    #[test]
    fn spawn_watch_stays_quiet_before_deadline() {
        let mut app = App::new();
        app.watch_spawn("cchub-w-2".into(), "claude".into(), "/tmp".into());
        let fired = app.check_spawn_watches(&[], Instant::now());
        assert!(!fired);
        assert_eq!(app.spawn_watches.len(), 1);
        assert!(app.status_msg.is_none());
    }

    // No session ever mapped to the watched tmux name: past the deadline the
    // watch fires once, sets a diagnosis status, and is dropped.
    #[test]
    fn spawn_watch_fires_diagnosis_after_timeout() {
        let mut app = App::new();
        app.watch_spawn(
            "cchub-watchtest-missing".into(),
            "claude".into(),
            "/tmp".into(),
        );
        let fired = app.check_spawn_watches(&[], Instant::now() + 2 * SPAWN_WATCH_TIMEOUT);
        assert!(fired);
        assert!(app.spawn_watches.is_empty());
        let (msg, _) = app.status_msg.as_ref().expect("diagnosis status");
        assert!(
            msg.contains("cchub-watchtest-missing"),
            "status should name the tmux session: {}",
            msg
        );
    }

    // A spawn is visible (and focused) the moment the watch is armed: a
    // Starting placeholder card in the spawn cwd's group, no scan needed.
    #[test]
    fn watch_spawn_shows_placeholder_immediately() {
        let mut app = App::new();
        app.watch_spawn("cchub-w-3".into(), "claude".into(), "/tmp/proj".into());
        let all: Vec<&SessionInfo> = app
            .sessions
            .groups
            .iter()
            .flat_map(|g| &g.sessions)
            .collect();
        assert_eq!(all.len(), 1);
        let p = all[0];
        assert_eq!(p.session_id, "spawning:cchub-w-3");
        assert_eq!(p.state, SessionState::Starting);
        assert_eq!(p.cwd, "/tmp/proj");
        assert_eq!(p.tmux_session.as_deref(), Some("cchub-w-3"));
        assert_eq!(
            app.selected_session_id().as_deref(),
            Some("spawning:cchub-w-3")
        );
    }

    // The placeholder must sit where the real card will land: after the
    // group's active sessions, at the head of the idle band (a fresh spawn
    // first scans in as Idle and the scanner orders newest-first within a
    // bucket). Pinning it to the top of the group made the card jump to the
    // idle band once the scanner took over.
    #[test]
    fn placeholder_sorts_into_the_idle_band() {
        let mut app = App::new();
        app.update_sessions(vec![
            fake_session("cchub-active", SessionState::Processing),
            fake_session("cchub-idle", SessionState::Idle),
        ]);
        app.watch_spawn("cchub-w-6".into(), "claude".into(), "/tmp".into());
        let ids: Vec<&str> = app.sessions.groups[0]
            .sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["cchub-active", "spawning:cchub-w-6", "cchub-idle"]
        );
    }

    // The scanner reporting a session hosted by the watched tmux name swaps
    // the placeholder for the real card on the same tick.
    #[test]
    fn placeholder_replaced_by_real_session() {
        let mut app = App::new();
        app.watch_spawn("cchub-w-4".into(), "claude".into(), "/tmp".into());
        let real = fake_session("cchub-w-4", SessionState::Idle);
        assert!(app.update_sessions(vec![real]));
        let ids: Vec<String> = app
            .sessions
            .groups
            .iter()
            .flat_map(|g| g.sessions.iter().map(|s| s.session_id.clone()))
            .collect();
        assert_eq!(ids, vec!["cchub-w-4".to_string()]);
        assert!(app.spawn_watches.is_empty());
    }

    // An expired watch takes its placeholder with it — the diagnosis status
    // is the only trace of the failed spawn.
    #[test]
    fn placeholder_dropped_when_watch_expires() {
        let mut app = App::new();
        app.watch_spawn("cchub-w-5".into(), "claude".into(), "/tmp".into());
        app.check_spawn_watches(&[], Instant::now() + 2 * SPAWN_WATCH_TIMEOUT);
        app.rebuild_groups();
        assert!(app.sessions.groups.is_empty());
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
    fn build_groups_tie_breaks_equal_names_by_cwd() {
        let app = App::new();
        // Two projects share the basename "api" but live at different paths,
        // so `name` alone ties. The sort must be deterministic (by cwd), not
        // dependent on HashMap iteration order.
        let mut work = fake_session("s-work", SessionState::Idle);
        work.cwd = "/home/work/api".into();
        work.project_name = "api".into();
        let mut personal = fake_session("s-personal", SessionState::Idle);
        personal.cwd = "/home/personal/api".into();
        personal.project_name = "api".into();

        let order = |gs: &[ProjectGroup]| gs.iter().map(|g| g.cwd.clone()).collect::<Vec<_>>();
        let a = app.build_groups(&[work.clone(), personal.clone()]);
        let b = app.build_groups(&[personal, work]);
        // Insertion order must not change the result.
        assert_eq!(order(&a), order(&b));
        assert_eq!(
            order(&a),
            vec![
                "/home/personal/api".to_string(),
                "/home/work/api".to_string()
            ],
        );
    }

    #[test]
    fn task_links_mark_cards_without_splitting_groups() {
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            let a = fake_session("s-a", SessionState::Idle);
            let b = fake_session("s-b", SessionState::Idle);
            let c = fake_session("s-c", SessionState::Idle);
            crate::session_tasks::link(
                "s-b",
                crate::session_tasks::TaskLink {
                    task_id: "tk-9".into(),
                    project_id: None,
                    title: "Fix auth".into(),
                },
            )
            .unwrap();
            app.session_task_links = crate::session_tasks::load();

            // A link is card metadata, not structure: all three sessions
            // stay in their one cwd group.
            let groups = app.build_groups(&[a, b, c]);
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].sessions.len(), 3);

            // The linked session resolves a badge; no live task with this
            // id, so it falls back to the sidecar's title snapshot and
            // marks itself stale. Unlinked sessions resolve none.
            let badge = app.task_badge("s-b").expect("badge");
            assert_eq!(badge.task_id, "tk-9");
            assert_eq!(badge.title, "Fix auth");
            assert!(badge.stale);
            assert!(app.task_badge("s-a").is_none());
        });
    }

    #[test]
    fn linked_sessions_lead_the_group_and_cluster_by_task() {
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            // An unlinked card leads the liveness order, two tk-1 members
            // straddle a liveness bucket boundary, and another unlinked
            // card trails.
            let b = fake_session("s-b", SessionState::Processing);
            let a = fake_session("s-a", SessionState::Processing);
            let c = fake_session("s-c", SessionState::Idle);
            let d = fake_session("s-d", SessionState::Idle);
            for sid in ["s-a", "s-c"] {
                crate::session_tasks::link(
                    sid,
                    crate::session_tasks::TaskLink {
                        task_id: "tk-1".into(),
                        project_id: None,
                        title: "Fix auth".into(),
                    },
                )
                .unwrap();
            }
            app.session_task_links = crate::session_tasks::load();

            let groups = app.build_groups(&[b, a, c, d]);
            assert_eq!(groups.len(), 1);
            let order: Vec<&str> = groups[0]
                .sessions
                .iter()
                .map(|s| s.session_id.as_str())
                .collect();
            // The tk-1 cluster jumps ahead of the unlinked s-b even though
            // s-b is first in the liveness order, and s-c is pulled up
            // behind its anchor across the bucket boundary; the unlinked
            // cards follow in their relative order.
            assert_eq!(order, vec!["s-a", "s-c", "s-b", "s-d"]);
        });
    }

    #[test]
    fn task_clusters_order_by_liveness_then_priority() {
        crate::test_util::with_temp_home(|| {
            use crate::orchestrator::TaskPriority;
            let mut app = App::new();
            let low = app.tasks.board.add("low task").unwrap().unwrap();
            app.tasks
                .board
                .set_priority(&low, TaskPriority::P4)
                .unwrap();
            let high = app.tasks.board.add("high task").unwrap().unwrap();
            app.tasks
                .board
                .set_priority(&high, TaskPriority::P1)
                .unwrap();

            // The high-priority cluster is idle, so it trails both active
            // clusters despite its P1 — liveness buckets first. Within the
            // active bucket priority takes over: P4 beats the unreadable
            // (gone) task, which has no priority and ranks last there.
            let gone_s = fake_session("s-gone", SessionState::Processing);
            let low_s = fake_session("s-low", SessionState::Processing);
            let high_s = fake_session("s-high", SessionState::Idle);
            let unlinked = fake_session("s-plain", SessionState::Processing);
            for (sid, task_id) in [
                ("s-gone", "tk-deleted"),
                ("s-low", low.as_str()),
                ("s-high", high.as_str()),
            ] {
                crate::session_tasks::link(
                    sid,
                    crate::session_tasks::TaskLink {
                        task_id: task_id.into(),
                        project_id: None,
                        title: String::new(),
                    },
                )
                .unwrap();
            }
            app.session_task_links = crate::session_tasks::load();

            let groups = app.build_groups(&[gone_s, low_s, high_s, unlinked]);
            assert_eq!(groups.len(), 1);
            let order: Vec<&str> = groups[0]
                .sessions
                .iter()
                .map(|s| s.session_id.as_str())
                .collect();
            assert_eq!(order, vec!["s-low", "s-gone", "s-high", "s-plain"]);
        });
    }

    #[test]
    fn task_link_candidates_band_in_tasks_board_column_order() {
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            use crate::orchestrator::TaskStatus;
            // Insert out of band order so the sort has to do the work.
            let done = app.tasks.board.add("done task").unwrap().unwrap();
            app.tasks.board.set_status(&done, TaskStatus::Done).unwrap();
            let running = app.tasks.board.add("running task").unwrap().unwrap();
            app.tasks
                .board
                .set_status(&running, TaskStatus::Running)
                .unwrap();
            let todo = app.tasks.board.add("todo task").unwrap().unwrap();
            let planning = app.tasks.board.add("planning task").unwrap().unwrap();
            app.tasks
                .board
                .set_status(&planning, TaskStatus::Planning)
                .unwrap();

            let session = fake_session("s-1", SessionState::Idle);
            let choices = app.task_link_candidates(&session);
            let labels: Vec<&str> = choices.iter().map(|c| c.label.as_str()).collect();
            assert_eq!(
                labels,
                vec!["todo task", "planning task", "running task", "done task"]
            );
            // Details carry the board label (not the wire name), and the
            // status rides along for the renderer's coloring.
            assert_eq!(choices[0].detail, "To-Do");
            assert_eq!(choices[0].status, Some(TaskStatus::Backlog));
            assert_eq!(choices[2].detail, "In Progress");
            let _ = todo;
        });
    }

    #[test]
    fn task_badge_prefers_live_title_and_dims_done() {
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            let id = app.tasks.board.add("Ship the parser").unwrap().unwrap();
            crate::session_tasks::link(
                "s-linked",
                crate::session_tasks::TaskLink {
                    task_id: id.clone(),
                    project_id: None,
                    title: "old snapshot".into(),
                },
            )
            .unwrap();
            app.session_task_links = crate::session_tasks::load();

            let badge = app.task_badge("s-linked").expect("badge");
            // Live board task wins over the sidecar snapshot.
            assert_eq!(badge.title, "Ship the parser");
            assert!(!badge.stale);

            // A Done task keeps the badge but dims it.
            app.tasks
                .board
                .set_status(&id, crate::orchestrator::TaskStatus::Done)
                .unwrap();
            assert!(app.task_badge("s-linked").unwrap().stale);
        });
    }

    #[test]
    fn ack_survives_rebuild() {
        let mut app = App::new();
        let mut s = fake_session("only", SessionState::WaitingForInput);
        s.tmux_session = None;
        assert!(app.update_sessions(vec![s]));
        app.sessions.sel_group = 0;
        app.sessions.sel_in_group = 0;

        assert!(app.ack_selected());
        assert_eq!(
            app.selected_session_info().map(|s| s.state.clone()),
            Some(SessionState::Idle)
        );

        // A rebuild before the next scan (here via a filter toggle) must not
        // revert the ack — the mutation is mirrored onto last_sessions.
        app.toggle_show_orch_workers();
        assert_eq!(
            app.selected_session_info().map(|s| s.state.clone()),
            Some(SessionState::Idle),
            "ack must survive a rebuild before the next scan"
        );
    }

    #[test]
    fn ack_reslots_card_into_idle_bucket_immediately() {
        let mut app = App::new();
        let mut a = fake_session("A", SessionState::WaitingForInput);
        a.tmux_session = None;
        let mut b = fake_session("B", SessionState::WaitingForInput);
        b.tmux_session = None;
        assert!(app.update_sessions(vec![a, b]));
        assert_eq!(app.selected_session_id().as_deref(), Some("A"));

        // Acking A downgrades it to Idle, which must push it behind the
        // still-active B right away — not on the next scan tick — with the
        // cursor following the card.
        assert!(app.ack_selected());
        let order: Vec<&str> = app.sessions.groups[0]
            .sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert_eq!(order, vec!["B", "A"]);
        assert_eq!(app.selected_session_id().as_deref(), Some("A"));
        assert_eq!(
            app.selected_session_info().map(|s| s.state.clone()),
            Some(SessionState::Idle)
        );
    }

    #[test]
    fn build_groups_orders_acked_idle_behind_active_sessions() {
        let app = App::new();
        // Simulate the post-scan ack downgrade: the flat list arrives in
        // scanner order (both were active when sorted), but one is Idle by
        // the time groups are built. build_groups must re-bucket it last.
        let mut idle = fake_session("acked", SessionState::Idle);
        idle.tmux_session = None;
        let mut active = fake_session("active", SessionState::Processing);
        active.tmux_session = None;
        let groups = app.build_groups(&[idle, active]);
        let order: Vec<&str> = groups[0]
            .sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert_eq!(order, vec!["active", "acked"]);
    }

    #[test]
    fn toggle_sessions_layout_cycles_and_resets_scroll() {
        let mut app = App::new();
        app.render.grid_scroll = 7;
        app.toggle_sessions_layout();
        assert_eq!(app.sessions.layout, SessionsLayout::List);
        assert_eq!(
            app.render.grid_scroll, 0,
            "a grid scroll offset can strand the shorter list viewport"
        );
        app.toggle_sessions_layout();
        assert_eq!(app.sessions.layout, SessionsLayout::Grid);
    }

    #[test]
    fn list_layout_nav_is_linear() {
        let mut app = App::new();
        let mut a = fake_session("A", SessionState::Processing);
        a.tmux_session = None;
        let mut b = fake_session("B", SessionState::Processing);
        b.tmux_session = None;
        let mut c = fake_session("C", SessionState::Processing);
        c.tmux_session = None;
        assert!(app.update_sessions(vec![a, b, c]));
        app.sessions.layout = SessionsLayout::List;
        // Even with the grid reporting 3 columns, list nav moves one row at
        // a time — down and right are the same linear step.
        app.render.grid_cols = 3;
        app.execute(Command::Sessions(SessionsCommand::NavDown));
        assert_eq!(app.sessions.sel_in_group, 1);
        app.execute(Command::Sessions(SessionsCommand::NavRight));
        assert_eq!(app.sessions.sel_in_group, 2);
        app.execute(Command::Sessions(SessionsCommand::NavLeft));
        assert_eq!(app.sessions.sel_in_group, 1);
        app.execute(Command::Sessions(SessionsCommand::NavUp));
        assert_eq!(app.sessions.sel_in_group, 0);
    }

    #[test]
    fn rebuild_revealed_session_does_not_teleport_cursor() {
        let mut app = App::new();
        let mut a = fake_session("A", SessionState::WaitingForInput);
        a.tmux_session = None;
        let mut b = fake_session("B", SessionState::Inactive);
        b.tmux_session = None;

        // First scan: B is inactive and hidden, so only A is visible/known.
        assert!(app.update_sessions(vec![a.clone(), b.clone()]));
        assert_eq!(app.selected_session_id().as_deref(), Some("A"));

        // Toggling show_inactive reveals B via a rebuild (no scan). B must get
        // registered as known so it isn't mistaken for a fresh arrival later.
        app.toggle_show_inactive();

        // Next scan adds a genuinely new session C. The focus jump must land
        // on C, not teleport onto B — which the user has already been seeing.
        let mut c = fake_session("C", SessionState::WaitingForInput);
        c.tmux_session = None;
        assert!(app.update_sessions(vec![a, b, c]));
        assert_eq!(
            app.selected_session_id().as_deref(),
            Some("C"),
            "focus jump should target the new session, not the revealed one"
        );
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
        t.project_root = Some(home.path().join("repo"));
        std::fs::create_dir_all(t.project_root.as_deref().unwrap()).unwrap();
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
        t.project_root = Some(home.path().join("repo"));
        std::fs::create_dir_all(t.project_root.as_deref().unwrap()).unwrap();
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
        t.project_root = Some(home.path().join("repo"));
        std::fs::create_dir_all(t.project_root.as_deref().unwrap()).unwrap();
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

        // b flips between active flavors: same bucket, same slots —
        // content-only update. (Waking from Idle *does* re-slot; that case is
        // covered by waking_session_moves_ahead_of_idle_ones.)
        let changed = app.update_sessions(vec![
            fake_session("a", SessionState::Processing),
            fake_session("b", SessionState::WaitingForInput),
            fake_session("c", SessionState::Processing),
        ]);
        assert!(changed, "a state flip is a visible change");
        let changed = app.update_sessions(vec![
            fake_session("a", SessionState::Processing),
            fake_session("b", SessionState::Processing),
            fake_session("c", SessionState::Processing),
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
    fn waking_session_moves_ahead_of_idle_ones() {
        let mut app = App::new();
        seed_three(&mut app);
        app.sessions.move_right();
        assert_eq!(app.selected_session_id().as_deref(), Some("b"));

        // b wakes up: it leaves the idle bucket and re-slots ahead of the
        // still-idle a and c; the cursor follows b's id to its new slot.
        let changed = app.update_sessions(vec![
            fake_session("a", SessionState::Idle),
            fake_session("b", SessionState::Processing),
            fake_session("c", SessionState::Idle),
        ]);
        assert!(changed, "waking is a visible change");
        assert_eq!(grid_ids(&app), ["b", "a", "c"]);
        assert_eq!(app.selected_session_id().as_deref(), Some("b"));
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

    // `$HOME`-redirected (PersonalBoard/Bookmarks persist on mutation), so
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
                let id = app.tasks.board.add("t").unwrap().unwrap();
                app.tasks
                    .board
                    .assign(&id, "/tmp/recent", "claude", "mux-1")
                    .unwrap();

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
                let id = app.tasks.board.add("do the thing").unwrap().unwrap();
                // A previous assignment on p-b: reopening the picker must
                // land the cursor there, not on the first candidate.
                app.tasks
                    .board
                    .assign(&id, "/tmp/p-b", "claude", "mux-1")
                    .unwrap();
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
                let prev = app.tasks.board.add("earlier").unwrap().unwrap();
                app.tasks
                    .board
                    .assign(&prev, "/tmp/p-b", "claude", "mux-1")
                    .unwrap();
                let id = app.tasks.board.add("next").unwrap().unwrap();
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
                let prev = app.tasks.board.add("earlier").unwrap().unwrap();
                app.tasks
                    .board
                    .assign(&prev, "/tmp/gone", "claude", "mux-1")
                    .unwrap();
                app.tasks.board.remove(&prev).unwrap();
                let id = app.tasks.board.add("next").unwrap().unwrap();
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
                let id = app.tasks.board.add("t").unwrap().unwrap();
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
                let id = app.tasks.board.add("t").unwrap().unwrap();
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
                let prev = app.tasks.board.add("broad question").unwrap().unwrap();
                app.tasks
                    .board
                    .assign(&prev, &home, "claude", "mux-1")
                    .unwrap();
                let id = app.tasks.board.add("next").unwrap().unwrap();
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
                let id = app.tasks.board.add("t").unwrap().unwrap();
                app.tasks.board.set_status(&id, TaskStatus::Done).unwrap();
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

                let id = app.tasks.board.add("old text").unwrap().unwrap();
                app.focus_task(&id);
                assert!(app.enter_task_rename());
                assert_eq!(app.view, View::TaskInput);
                assert_eq!(app.tasks.input, "old text");

                app.tasks.input = "new text".into();
                assert!(app.submit_task_input());
                assert_eq!(app.tasks.board.get(&id).unwrap().prompt, "new text");
                assert_eq!(app.view, View::Grid);
                assert!(app.tasks.renaming.is_none());

                // A whitespace-only commit must not wipe the task.
                assert!(app.enter_task_rename());
                app.tasks.input = "   ".into();
                assert!(!app.submit_task_input());
                assert_eq!(app.tasks.board.get(&id).unwrap().prompt, "new text");
            });
        }

        #[test]
        fn rename_does_not_touch_status_or_binding() {
            with_temp_home(|| {
                let mut app = App::new();
                let id = app.tasks.board.add("t").unwrap().unwrap();
                app.tasks
                    .board
                    .assign(&id, "/tmp/proj", "claude", "mux-1")
                    .unwrap();
                app.focus_task(&id);
                assert!(app.enter_task_rename());
                app.tasks.input = "sharper wording".into();
                assert!(app.submit_task_input());
                let t = app.tasks.board.get(&id).unwrap();
                assert_eq!(t.prompt, "sharper wording");
                assert_eq!(t.status, TaskStatus::Planning);
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
        fn space_on_planning_without_session_stays_planning() {
            with_temp_home(|| {
                let mut app = App::new();
                let id = app.tasks.board.add("plan me").unwrap().unwrap();
                app.tasks
                    .board
                    .assign(&id, "/tmp", "claude", "mux-dead")
                    .unwrap();
                app.focus_task(&id);
                // No live mux and no resolved session id: nothing to deliver
                // the proceed prompt to, so the card must not move — an In
                // Progress card with no agent working it would be a lie.
                let msg = app.task_space_action().unwrap();
                assert!(msg.contains("press s to re-assign"), "msg: {msg}");
                assert_eq!(
                    app.tasks.board.get(&id).unwrap().status,
                    TaskStatus::Planning
                );
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
                // re-entered — live re-sorting is how the Sessions grid used
                // to swap cards under the cursor.
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
                app.sessions.last_sessions =
                    vec![fake_session("mux-b", SessionState::WaitingForInput)];
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
        fn raising_priority_moves_card_and_cursor_follows() {
            with_temp_home(|| {
                let mut app = App::new();
                app.tasks.board.add("a").unwrap().unwrap();
                app.tasks.board.add("b").unwrap().unwrap();
                let c = app.tasks.board.add("c").unwrap().unwrap();
                app.focus_task(&c);
                assert_eq!((app.tasks.col, app.tasks.row), (0, 2));

                // Bumping c to P1 floats it to the top; the cursor rides along.
                app.set_selected_task_priority(TaskPriority::P1);
                assert_eq!((app.tasks.col, app.tasks.row), (0, 0));
                assert_eq!(app.selected_board_task().unwrap().task_id, c);
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
        fn toggle_done_completes_assigned_task_and_keeps_binding() {
            with_temp_home(|| {
                let mut app = App::new();
                let id = app.tasks.board.add("ship it").unwrap().unwrap();
                app.tasks
                    .board
                    .assign(&id, "/tmp", "claude", "mux-dead")
                    .unwrap();
                app.focus_task(&id);
                // No live mux session in the test env: the close is skipped
                // and the status stays the plain "done" line.
                assert_eq!(app.toggle_task_done().unwrap(), "done: ship it");
                let t = app.tasks.board.get(&id).unwrap();
                assert_eq!(t.status, TaskStatus::Done);
                // The binding survives completion so `f` can still resume.
                assert_eq!(t.tmux.as_deref(), Some("mux-dead"));
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
    }

    #[cfg(unix)]
    mod manual_moves {
        use super::*;
        use crate::test_util::with_temp_home;

        #[test]
        fn l_walks_todo_to_done_and_h_back_skipping_planning() {
            with_temp_home(|| {
                let mut app = App::new();
                let id = app.tasks.board.add("hands-on work").unwrap().unwrap();
                app.focus_task(&id);
                // Right: To-Do → In Progress (Planning is agent-owned, so
                // the manual move hops over it).
                let msg = app.move_selected_task(1).unwrap();
                assert!(msg.contains("In Progress"), "msg: {msg}");
                assert_eq!(
                    app.tasks.board.get(&id).unwrap().status,
                    TaskStatus::Running
                );
                // The cursor rides with the card.
                assert_eq!(app.selected_board_task().unwrap().task_id, id);
                // Right again: → Done, stamping done_at exactly like Space.
                let msg = app.move_selected_task(1).unwrap();
                assert!(msg.starts_with("done:"), "msg: {msg}");
                let t = app.tasks.board.get(&id).unwrap();
                assert_eq!(t.status, TaskStatus::Done);
                assert!(t.done_at.is_some());
                // Off the right edge: refused.
                app.focus_task(&id);
                assert!(app.move_selected_task(1).is_none());
                // Left: Done → In Progress reopens (done_at cleared).
                app.move_selected_task(-1).unwrap();
                let t = app.tasks.board.get(&id).unwrap();
                assert_eq!(t.status, TaskStatus::Running);
                assert!(t.done_at.is_none());
                // Left again: → To-Do; then off the left edge.
                app.move_selected_task(-1).unwrap();
                assert_eq!(
                    app.tasks.board.get(&id).unwrap().status,
                    TaskStatus::Backlog
                );
                assert!(app.move_selected_task(-1).is_none());
            });
        }

        #[test]
        fn planning_card_moves_out_without_proceed_prompt() {
            with_temp_home(|| {
                let mut app = App::new();
                let id = app.tasks.board.add("agent task").unwrap().unwrap();
                app.tasks
                    .board
                    .assign(&id, "/tmp", "claude", "mux-x")
                    .unwrap();
                app.focus_task(&id);
                // Right: Planning → In Progress, but the agent is NOT told
                // to proceed — approving stays Space's job.
                let msg = app.move_selected_task(1).unwrap();
                assert!(msg.contains("not told to proceed"), "msg: {msg}");
                let t = app.tasks.board.get(&id).unwrap();
                assert_eq!(t.status, TaskStatus::Running);
                assert_eq!(t.tmux.as_deref(), Some("mux-x"));
                // A Planning card can also be parked back in To-Do.
                app.tasks
                    .board
                    .set_status(&id, TaskStatus::Planning)
                    .unwrap();
                app.focus_task(&id);
                app.move_selected_task(-1).unwrap();
                assert_eq!(
                    app.tasks.board.get(&id).unwrap().status,
                    TaskStatus::Backlog
                );
            });
        }
    }

    #[cfg(unix)]
    mod board_filter {
        use super::*;
        use crate::test_util::with_temp_home;

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

    #[cfg(unix)]
    mod task_undo {
        use super::*;
        use crate::test_util::with_temp_home;

        #[test]
        fn undo_restores_deleted_task_once() {
            with_temp_home(|| {
                let mut app = App::new();
                let id = app.tasks.board.add("precious").unwrap().unwrap();
                app.focus_task(&id);
                let msg = app.delete_selected_task().unwrap();
                assert!(msg.contains("u undoes"), "msg: {msg}");
                assert!(app.tasks.board.get(&id).is_none());
                assert_eq!(app.undo_task_delete().unwrap(), "restored 1 task");
                assert_eq!(app.tasks.board.get(&id).unwrap().prompt, "precious");
                // The slot is one batch deep: a second undo finds nothing.
                assert!(app.undo_task_delete().is_none());
            });
        }

        #[test]
        fn undo_restores_cleared_done_batch_with_statuses() {
            with_temp_home(|| {
                let mut app = App::new();
                let a = app.tasks.board.add("a").unwrap().unwrap();
                let b = app.tasks.board.add("b").unwrap().unwrap();
                app.tasks.board.set_status(&a, TaskStatus::Done).unwrap();
                app.tasks.board.set_status(&b, TaskStatus::Done).unwrap();
                app.clear_done_tasks();
                assert!(app.tasks.board.tasks().is_empty());
                assert_eq!(app.undo_task_delete().unwrap(), "restored 2 tasks");
                // They come back Done, not To-Do — undo is not a reopen.
                assert_eq!(app.tasks.board.get(&a).unwrap().status, TaskStatus::Done);
                assert_eq!(app.tasks.board.get(&b).unwrap().status, TaskStatus::Done);
            });
        }
    }

    #[cfg(unix)]
    mod quick_add {
        use super::*;
        use crate::test_util::with_temp_home;

        #[test]
        fn add_popup_parses_tags_and_priority_inline() {
            with_temp_home(|| {
                let mut app = App::new();
                app.enter_task_input();
                app.tasks.input = "fix the parser #bug #api !1".into();
                assert!(app.submit_task_input());
                let t = app.selected_board_task().unwrap();
                assert_eq!(t.prompt, "fix the parser");
                assert_eq!(t.tags, vec!["bug", "api"]);
                assert_eq!(t.priority, TaskPriority::P1);
            });
        }

        #[test]
        fn syntax_only_input_adds_nothing_and_rename_is_verbatim() {
            with_temp_home(|| {
                let mut app = App::new();
                app.enter_task_input();
                app.tasks.input = "#bug !1".into();
                assert!(!app.submit_task_input());
                assert!(app.tasks.board.tasks().is_empty());

                // Rename keeps the syntax as literal text — the sugar is
                // add-only, so hashes in existing task text survive edits.
                let id = app.tasks.board.add("plain").unwrap().unwrap();
                app.focus_task(&id);
                assert!(app.enter_task_rename());
                app.tasks.input = "now with #hash !1".into();
                assert!(app.submit_task_input());
                let t = app.tasks.board.get(&id).unwrap();
                assert_eq!(t.prompt, "now with #hash !1");
                assert!(t.tags.is_empty());
            });
        }
    }
}
