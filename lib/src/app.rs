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
use std::collections::{HashMap, HashSet, VecDeque};
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
    Backlog,
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

/// Pending registry-level project removal. Shown via the same
/// `ConfirmClose` view as task delete, distinguished by
/// [`App::pending_project_delete`] being `Some`.
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
    pub pending_project_delete: Option<PendingProjectDelete>,
    pub pending_task_restart: Option<PendingTaskRestart>,
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
    pub pending_dispatch: VecDeque<PendingDispatch>,
    pub show_inactive: bool,
    /// When false, the Sessions view hides any session whose tmux name is
    /// claimed by an orchestrator or worker in the current projects
    /// snapshot. Toggled with `W` so the user can drop into the raw view
    /// when something looks off.
    pub show_orch_workers: bool,
    pub projects: ProjectsSnapshot,
    /// Cursor in the Projects tab. `0..projects.len()` selects a project;
    /// task selection within the project lives in [`Self::projects_task_sel`].
    pub projects_sel: usize,
    pub projects_task_sel: usize,
    /// Kanban column cursor: 0=Planning, 1=Running, 2=Review, 3=Merging,
    /// 4=Done. Drives which column [`Self::projects_task_sel`] indexes
    /// into.
    pub projects_col: usize,
    /// True while the folder picker / prompt input flow is creating a
    /// new project task (vs. spawning a regular session). Used to route
    /// the picker's space-pick and prompt-input's enter to the project
    /// flow instead of [`spawn::spawn_claude_session`].
    pub creating_project_task: bool,
    /// Cursor inside the Backlog popup, indexing into the selected
    /// project's backlog tasks. Reset on popup open. Backlog tasks live
    /// off the kanban (which starts at Planning) so this popup is the
    /// only way to see and start them.
    pub backlog_sel: usize,
    /// True while the folder picker is in "register a project, no task"
    /// mode (the `N` shortcut from the Projects view). Picking a folder
    /// just runs [`crate::orchestrator::ensure_project_registered`] and
    /// closes the picker — no orchestrator is spawned.
    pub registering_project_only: bool,
    /// cwd captured when the picker chose a folder in Projects mode. Held
    /// until the user submits the task prompt; consumed in
    /// [`Self::submit_project_task`].
    pub projects_pending_cwd: Option<String>,
    /// Agent override for the pending project task. `None` means "use the
    /// project default at spawn time"; `Some(id)` is set by the user
    /// cycling backends in the prompt-input view via Tab.
    pub projects_pending_agent_id: Option<String>,
    /// Latest scan snapshot; drives [`rebuild_groups`].
    last_sessions: Vec<SessionInfo>,
    /// Session ids seen on the previous scan tick. `None` means the first
    /// scan hasn't happened yet — used to skip cursor-jump on initial load.
    known_session_ids: Option<HashSet<String>>,
    /// Cursor inside the Projects "Result" popup, indexing into the
    /// selected task's `artifacts` vec. Reset on popup open.
    pub result_artifact_sel: usize,
    /// Vertical scroll offset (in unwrapped lines) for the Projects "Result"
    /// popup body. Adjusted by the renderer to keep the selected card
    /// visible, and by `PgUp`/`PgDn` for free scrolling. Reset on open.
    pub result_scroll: u16,
    /// When true, the currently-selected evidence card renders with an
    /// enlarged body so the user can see more of its content inline. The
    /// flag follows the j/k cursor — it is not tied to a specific artifact.
    pub result_artifact_expanded: bool,
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
    /// Task id we want the kanban cursor to jump to once the next
    /// ProjectsSnapshot includes it. Set when the user starts a Backlog
    /// task; cleared in update_projects once focus has moved (or when
    /// the budget below runs out).
    pub pending_focus_task_id: Option<String>,
    /// Snapshot ticks remaining to find pending_focus_task_id before we
    /// give up with a soft toast. Started at 5 — the fs-watcher-driven
    /// scan typically lands within one tick, but the periodic 2s ticker
    /// can interleave, so allow a few attempts.
    pub pending_focus_budget: u8,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
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
            pending_project_delete: None,
            pending_task_restart: None,
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
            pending_dispatch: VecDeque::new(),
            show_inactive: false,
            show_orch_workers: false,
            projects: ProjectsSnapshot::empty(),
            projects_sel: 0,
            projects_task_sel: 0,
            projects_col: 0,
            backlog_sel: 0,
            creating_project_task: false,
            registering_project_only: false,
            projects_pending_cwd: None,
            projects_pending_agent_id: None,
            last_sessions: Vec::new(),
            known_session_ids: None,
            result_artifact_sel: 0,
            result_scroll: 0,
            result_artifact_expanded: false,
            image_picker: None,
            artifact_images: HashMap::new(),
            artifact_image_failed: HashSet::new(),
            pending_focus_task_id: None,
            pending_focus_budget: 0,
        }
    }

    pub fn toggle_show_inactive(&mut self) {
        self.show_inactive = !self.show_inactive;
        self.rebuild_groups();
    }

    pub fn toggle_show_orch_workers(&mut self) {
        self.show_orch_workers = !self.show_orch_workers;
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
        // Track the focused task by id so a status transition (Running →
        // Review etc.) carries the cursor across columns. Mirrors the
        // prev_sid trick in `update_metrics`.
        let prev_task_id = self.selected_project_task().map(|t| t.task_id.clone());
        let first_load = self.projects.projects.is_empty();
        self.projects = snap;
        if let Some(pid) = prev_pid {
            if let Some(idx) = self.projects.projects.iter().position(|p| p.id == pid) {
                self.projects_sel = idx;
            }
        }
        // If the task is gone, fall through and let clamp handle the row.
        if let Some(task_id) = prev_task_id {
            self.focus_task(&task_id);
        }
        // Jump-if-empty only on the very first load — once the user is in the
        // tab, an empty focused column means they explicitly navigated there
        // (or a task drained out of it), and silently overriding their
        // selection on every rescan is the bug we're avoiding.
        if first_load {
            self.clamp_projects_cursor_jump_if_empty();
        } else {
            self.clamp_projects_cursor();
        }
        if let Some(task_id) = self.pending_focus_task_id.clone() {
            if let Some(col) = self.focus_task(&task_id) {
                self.set_status(format!(
                    "started {} — focus moved to {}",
                    crate::orchestrator::short_task_id(&task_id),
                    kanban_col_name(col),
                ));
                self.pending_focus_task_id = None;
                self.pending_focus_budget = 0;
            } else {
                self.pending_focus_budget = self.pending_focus_budget.saturating_sub(1);
                if self.pending_focus_budget == 0 {
                    self.set_status(format!(
                        "started {} — orchestrator booting; cursor unchanged",
                        crate::orchestrator::short_task_id(&task_id),
                    ));
                    self.pending_focus_task_id = None;
                }
            }
        }
        // Newly-discovered orchestrator/worker tmux names need to disappear
        // from the Sessions view immediately; without this the hide flag
        // would only take effect on the next session scan.
        self.rebuild_groups();
    }

    /// Search the focused project's kanban columns for `task_id`. If found,
    /// move `projects_col` / `projects_task_sel` onto it and return the
    /// column index. Returns None if not found in any column (or if no
    /// project is selected).
    pub fn focus_task(&mut self, task_id: &str) -> Option<usize> {
        for col in 0..5 {
            let tasks = self.kanban_column_tasks(col);
            if let Some(row) = tasks.iter().position(|t| t.task_id == task_id) {
                self.projects_col = col;
                self.projects_task_sel = row;
                return Some(col);
            }
        }
        None
    }

    fn clamp_projects_cursor(&mut self) {
        self.clamp_projects_cursor_inner(false);
    }

    /// Like [`Self::clamp_projects_cursor`] but, if the focused column ends up
    /// empty, jumps to the first non-empty column. Use at user-driven entry
    /// points (project switch, approve_review) — not on every rescan, since
    /// that would override an explicit column selection on the next tick.
    fn clamp_projects_cursor_jump_if_empty(&mut self) {
        self.clamp_projects_cursor_inner(true);
    }

    fn clamp_projects_cursor_inner(&mut self, jump_if_empty: bool) {
        let n = self.projects.projects.len();
        if n == 0 {
            self.projects_sel = 0;
            self.projects_task_sel = 0;
            self.projects_col = 0;
            return;
        }
        if self.projects_sel >= n {
            self.projects_sel = n - 1;
        }
        if self.projects_col > 4 {
            self.projects_col = 4;
        }
        if jump_if_empty && self.kanban_column_len(self.projects_col) == 0 {
            for col in 0..5 {
                if self.kanban_column_len(col) > 0 {
                    self.projects_col = col;
                    break;
                }
            }
        }
        let col_count = self.kanban_column_len(self.projects_col);
        if col_count == 0 {
            self.projects_task_sel = 0;
        } else if self.projects_task_sel >= col_count {
            self.projects_task_sel = col_count - 1;
        }
    }

    /// Cycle through projects (top chip strip).
    pub fn projects_move_down(&mut self) {
        if self.projects.projects.is_empty() {
            return;
        }
        self.projects_sel = (self.projects_sel + 1).min(self.projects.projects.len() - 1);
    }

    pub fn projects_move_up(&mut self) {
        self.projects_sel = self.projects_sel.saturating_sub(1);
    }

    /// Move cursor down within the current kanban column.
    pub fn projects_task_next(&mut self) {
        let col_count = self.kanban_column_len(self.projects_col);
        if col_count == 0 {
            return;
        }
        self.projects_task_sel = (self.projects_task_sel + 1).min(col_count - 1);
    }

    pub fn projects_task_prev(&mut self) {
        self.projects_task_sel = self.projects_task_sel.saturating_sub(1);
    }

    /// Move kanban cursor one column right (Planning → Running → Review
    /// → Merging → Done). Clamps the row cursor to the new
    /// column's length.
    pub fn projects_col_right(&mut self) {
        if self.projects_col < 4 {
            self.projects_col += 1;
            self.projects_task_sel = 0;
        }
    }

    pub fn projects_col_left(&mut self) {
        if self.projects_col > 0 {
            self.projects_col -= 1;
            self.projects_task_sel = 0;
        }
    }

    pub fn selected_project(&self) -> Option<&crate::orchestrator::Project> {
        self.projects.projects.get(self.projects_sel)
    }

    /// Tasks in the currently-selected project that match the given
    /// kanban column. Columns are derived from `TaskStatus` + worker
    /// presence: a Running task with no workers is in "Planning"
    /// (orchestrator is still decomposing); Running + workers is true
    /// Running; Review/Merging/Done map straight from status.
    ///
    /// Indices: 0=Planning, 1=Running, 2=Review, 3=Merging, 4=Done.
    /// Order matches the underlying `tasks` Vec (already sorted
    /// newest-first by the orchestrator).
    pub fn kanban_column_tasks(&self, col: usize) -> Vec<&crate::orchestrator::TaskState> {
        let Some(p) = self.selected_project() else {
            return Vec::new();
        };
        let Some(tasks) = self.projects.tasks.get(&p.id) else {
            return Vec::new();
        };
        use crate::orchestrator::TaskStatus;
        tasks
            .iter()
            .filter(|t| match col {
                0 => t.status == TaskStatus::Running && t.workers.is_empty(),
                1 => t.status == TaskStatus::Running && !t.workers.is_empty(),
                2 => t.status == TaskStatus::Review,
                3 => t.status == TaskStatus::Merging,
                _ => t.status == TaskStatus::Done,
            })
            .map(|t| t.as_ref())
            .collect()
    }

    pub fn kanban_column_len(&self, col: usize) -> usize {
        self.kanban_column_tasks(col).len()
    }

    /// Backlog tasks for the currently-selected project, in scan order
    /// (newest first, same as the underlying `tasks` Vec). Backlog tasks
    /// don't appear in the kanban; the Backlog popup (`View::Backlog`) is
    /// where they're listed and started.
    pub fn backlog_tasks(&self) -> Vec<&crate::orchestrator::TaskState> {
        let Some(p) = self.selected_project() else {
            return Vec::new();
        };
        let Some(tasks) = self.projects.tasks.get(&p.id) else {
            return Vec::new();
        };
        use crate::orchestrator::TaskStatus;
        tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Backlog)
            .map(|t| t.as_ref())
            .collect()
    }

    pub fn open_backlog(&mut self) {
        self.backlog_sel = 0;
        self.view = View::Backlog;
    }

    pub fn close_backlog(&mut self) {
        self.view = View::Grid;
    }

    pub fn backlog_up(&mut self) {
        if self.backlog_sel > 0 {
            self.backlog_sel -= 1;
        }
    }

    pub fn backlog_down(&mut self) {
        let n = self.backlog_tasks().len();
        if n > 0 && self.backlog_sel + 1 < n {
            self.backlog_sel += 1;
        }
    }

    pub fn selected_backlog_task(&self) -> Option<&crate::orchestrator::TaskState> {
        self.backlog_tasks().get(self.backlog_sel).copied()
    }

    pub fn selected_project_task(&self) -> Option<&crate::orchestrator::TaskState> {
        let col = self.kanban_column_tasks(self.projects_col);
        col.get(self.projects_task_sel).copied()
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
        let project_root = t.project_root.clone();

        // Resolve the SHAs the user is approving — captured here rather
        // than in `pr approve` so we don't need a subprocess.
        let pr = match crate::pr::read_pr(&project_id, &task_id) {
            Ok(Some(p)) => p,
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
        let branch_sha = git_rev_parse_short(&project_root, &pr.branch);
        let base = crate::orchestrator::detect_main_branch(&project_root);
        let base_sha = git_rev_parse_short(&project_root, &base);

        if let Err(e) = crate::pr::update_pr(&project_id, &task_id, |p| {
            p.review_state = crate::pr::ReviewState::Approved;
            p.approved_at_branch_sha = branch_sha;
            p.approved_at_base_sha = base_sha;
        }) {
            self.set_status(format!("approve failed: {}", e));
            return ApproveOutcome::Failed;
        }

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

    /// `tmux_session_name → SessionInfo` over the latest scan. Built fresh
    /// per call so it always reflects [`Self::last_sessions`]. Used by the
    /// Projects view to enrich task cards with live agent state (context
    /// tokens, current tool, idle/processing/waiting).
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
        self.metrics_selected.and_then(|i| self.metrics_rows.get(i))
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
        self.registering_project_only = true;
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
        self.projects_pending_cwd = Some(cwd);
        self.projects_pending_agent_id = None;
        self.prompt_buffer.clear();
        self.dispatch_target = None;
        self.view = View::PromptInput;
    }

    /// Cycle the orchestrator agent for the pending project task to the
    /// next entry in the resolved-agents map. No-op when fewer than two
    /// agents are configured. Called from the prompt-input Tab handler.
    pub fn cycle_pending_agent_id(&mut self) {
        if self.projects_pending_cwd.is_none() {
            return;
        }
        let agents = config::get().resolved_agents();
        let ids: Vec<String> = agents.into_keys().collect();
        if ids.len() < 2 {
            return;
        }
        let current = self
            .projects_pending_agent_id
            .clone()
            .unwrap_or_else(|| config::get().default_orchestrator_agent_id());
        let idx = ids.iter().position(|id| id == &current).unwrap_or(0);
        let next = ids[(idx + 1) % ids.len()].clone();
        self.projects_pending_agent_id = Some(next);
    }

    /// Display label for the agent that will run the pending project task,
    /// resolving `None` to the configured default. Returns `None` outside
    /// the project-creation flow.
    pub fn pending_agent_label(&self) -> Option<String> {
        self.projects_pending_cwd.as_ref()?;
        Some(
            self.projects_pending_agent_id
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
        self.creating_project_task = false;
        self.registering_project_only = false;
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
        self.projects_pending_agent_id = None;
        self.creating_project_task = false;
        self.view = View::Grid;
    }

    /// True when the prompt input should be routed through the orchestrator
    /// project-task flow instead of the regular session-dispatch flow.
    pub fn prompt_input_for_project(&self) -> bool {
        self.projects_pending_cwd.is_some()
    }

    /// Consumes the pending cwd, prompt, and agent override, clears
    /// prompt input, returns the `(cwd, prompt, agent_id_override)` tuple.
    /// Returns `None` if no project task is pending. The override is
    /// `None` when the user didn't cycle off the configured default.
    pub fn submit_project_task(&mut self) -> Option<(String, String, Option<String>)> {
        let cwd = self.projects_pending_cwd.take()?;
        let agent_id = self.projects_pending_agent_id.take();
        self.creating_project_task = false;
        self.view = View::Grid;
        let prompt = std::mem::take(&mut self.prompt_buffer);
        Some((cwd, prompt, agent_id))
    }

    pub fn submit_prompt_input(&mut self) -> String {
        self.view = View::Grid;
        std::mem::take(&mut self.prompt_buffer)
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
        // Walk the unfiltered scan set, not `self.groups`: orchestrator
        // and worker tmux names are hidden from `groups` by the Sessions
        // view filter, so checking `groups` here would block dispatch
        // forever for the very sessions we need to dispatch into.
        let scanner_idle = self.last_sessions.iter().any(|s| {
            s.tmux_session.as_deref() == Some(pd.tmux.as_str()) && s.state == SessionState::Idle
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
                return DispatchAction::Send {
                    tmux: pd.tmux,
                    prompt: pd.prompt,
                };
            }
        }
        if pd.queued_at.elapsed() > config::get().ui.pending_dispatch_timeout() {
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
        self.pending_project_delete = None;
        self.pending_task_restart = None;
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
        let Some(p) = self.selected_project().cloned() else {
            self.set_status("no project selected".into());
            return;
        };
        let Some(task) = self.selected_project_task().cloned() else {
            self.set_status("no task selected — focus a task on the kanban first".into());
            return;
        };
        let status_label = match task.status {
            crate::orchestrator::TaskStatus::Running => "running",
            crate::orchestrator::TaskStatus::Review => "review",
            crate::orchestrator::TaskStatus::Merging => "merging",
            crate::orchestrator::TaskStatus::Done => "done",
            crate::orchestrator::TaskStatus::Backlog => "backlog",
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

    /// Stage removal of the currently-selected project from the cc-hub
    /// registry, mirroring [`Self::enter_confirm_task_delete`]. Surfaces task
    /// count in the prompt so the user sees how much state they're nuking.
    pub fn enter_confirm_project_delete(&mut self) {
        let Some(p) = self.selected_project().cloned() else {
            self.set_status("no project selected".into());
            return;
        };
        let n = self.projects.tasks.get(&p.id).map(|v| v.len()).unwrap_or(0);
        let display = format!("{} ({} task{})", p.name, n, if n == 1 { "" } else { "s" });
        self.pending_project_delete = Some(PendingProjectDelete {
            project_id: p.id.clone(),
            display,
        });
        self.view = View::ConfirmClose;
    }

    pub fn take_pending_project_delete(&mut self) -> Option<PendingProjectDelete> {
        self.view = View::Grid;
        self.pending_project_delete.take()
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
        let status_label = match task.status {
            crate::orchestrator::TaskStatus::Running => "running",
            crate::orchestrator::TaskStatus::Review => "review",
            crate::orchestrator::TaskStatus::Merging => "merging",
            crate::orchestrator::TaskStatus::Done => "done",
            crate::orchestrator::TaskStatus::Backlog => "backlog",
        };
        let display = format!(
            "{} — {} (task {})",
            p.name,
            status_label,
            crate::orchestrator::short_task_id(&task.task_id),
        );
        self.pending_task_restart = Some(PendingTaskRestart {
            project_id: p.id.clone(),
            task_id: task.task_id.clone(),
            display,
        });
        self.view = View::ConfirmClose;
    }

    pub fn take_pending_task_restart(&mut self) -> Option<PendingTaskRestart> {
        self.view = View::Grid;
        self.pending_task_restart.take()
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
        self.selected_session_info().map(|s| s.session_id.clone())
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
            let live_ids: HashSet<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
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
        let roles = self.projects.roles_by_tmux();

        let sessions: Vec<SessionInfo> = self
            .last_sessions
            .iter()
            .filter(|s| self.show_inactive || s.state != SessionState::Inactive)
            .filter(|s| {
                // Hide tmux sessions claimed by an orchestrator or worker
                // unless the user has asked to see them. Sessions without a
                // tmux name (legacy/manual launches) always show.
                if self.show_orch_workers {
                    return true;
                }
                match s.tmux_session.as_deref() {
                    Some(name) => !roles.contains_key(name),
                    None => true,
                }
            })
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
        self.groups.sort_by_key(|a| a.name.to_lowercase());

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
            let max_in = self.groups[self.sel_group].sessions.len().saturating_sub(1);
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
                target_pid,
                name,
                tmux
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
        self.result_scroll = 0;
        self.result_artifact_expanded = false;
        self.view = View::ProjectsResult;
        true
    }

    pub fn close_projects_result(&mut self) {
        self.view = View::Grid;
        self.result_artifact_sel = 0;
        self.result_scroll = 0;
        self.result_artifact_expanded = false;
    }

    pub fn toggle_result_artifact_expanded(&mut self) {
        if self.selected_result_artifact().is_none() {
            return;
        }
        self.result_artifact_expanded = !self.result_artifact_expanded;
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

    /// PgUp/PgDn handler. Negative steps scroll up; the renderer clamps the
    /// offset against content length so we never scroll past the end.
    pub fn result_scroll_by(&mut self, delta: i32) {
        let cur = self.result_scroll as i32;
        let next = (cur + delta).max(0);
        self.result_scroll = next.min(u16::MAX as i32) as u16;
    }

    /// The artifact under the popup cursor, if any. Used by the `c` and `o`
    /// keybinds to know what path to act on.
    pub fn selected_result_artifact(&self) -> Option<&crate::orchestrator::Artifact> {
        let t = self.selected_project_task()?;
        t.artifacts.get(self.result_artifact_sel)
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

fn git_rev_parse_short(root: &std::path::Path, rev: &str) -> Option<String> {
    let out = crate::orchestrator::run_git(root, &["rev-parse", rev]).ok()?;
    if !out.status_ok {
        return None;
    }
    Some(out.stdout.trim().to_string())
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
        let mut snap = ProjectsSnapshot::empty();
        let pid = p.id.clone();
        snap.projects.push(p);
        snap.tasks
            .insert(pid, tasks.into_iter().map(Arc::new).collect());
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
        app.last_sessions = vec![
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

        assert_eq!(app.projects_col, 1, "Running+workers should land in col 1");
        assert_eq!(
            app.selected_project_task().map(|t| t.task_id.clone()),
            Some("t-1".to_string()),
        );

        // Same task moves to Review (column 2). Cursor must follow.
        let snap2 = snapshot(p, vec![task("p-1", "t-1", TaskStatus::Review, false)]);
        app.update_projects(snap2);

        assert_eq!(
            app.projects_col, 2,
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
        assert_eq!(app.projects_col, 2);

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
        assert_eq!(app.projects_col, 2);

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
    fn projects_cursor_clamps_when_task_disappears() {
        let mut app = App::new();
        app.current_tab = Tab::Projects;

        let p = project("p-1");
        let snap1 = snapshot(
            p.clone(),
            vec![task("p-1", "t-1", TaskStatus::Running, true)],
        );
        app.update_projects(snap1);
        assert_eq!(app.projects_col, 1);

        // Task vanishes from the snapshot entirely.
        let snap2 = snapshot(p, Vec::new());
        app.update_projects(snap2);

        assert!(app.selected_project_task().is_none());
        assert_eq!(
            app.projects_col, 1,
            "column should stay where it was when task disappears",
        );
        assert_eq!(app.projects_task_sel, 0, "row should clamp to 0");
    }

    #[test]
    fn pending_focus_jumps_to_planning_when_task_appears() {
        let mut app = App::new();
        app.current_tab = Tab::Projects;
        let p = project("p-1");
        // Initial snapshot: empty (the task hasn't been written yet from
        // the orchestrator's POV, or is still in Backlog).
        app.update_projects(snapshot(p.clone(), Vec::new()));
        app.pending_focus_task_id = Some("t-new".to_string());
        app.pending_focus_budget = 5;
        // New snapshot: task appears as Running with no workers → Planning column.
        app.update_projects(snapshot(
            p,
            vec![task("p-1", "t-new", TaskStatus::Running, false)],
        ));
        assert_eq!(app.projects_col, 0, "cursor should land on Planning");
        assert_eq!(app.projects_task_sel, 0);
        assert!(
            app.pending_focus_task_id.is_none(),
            "pending should clear after success"
        );
    }

    #[test]
    fn pending_focus_budget_runs_out_when_task_never_arrives() {
        let mut app = App::new();
        app.current_tab = Tab::Projects;
        let p = project("p-1");
        app.update_projects(snapshot(p.clone(), Vec::new()));
        app.pending_focus_task_id = Some("t-ghost".to_string());
        app.pending_focus_budget = 2;
        // Two empty snapshots → budget exhausted, pending cleared.
        app.update_projects(snapshot(p.clone(), Vec::new()));
        assert!(app.pending_focus_task_id.is_some(), "still pending after 1");
        app.update_projects(snapshot(p, Vec::new()));
        assert!(
            app.pending_focus_task_id.is_none(),
            "cleared after budget=0"
        );
    }
}
