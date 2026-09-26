//! Top-level view routing: the focused tab, the active overlay, and the
//! generic popups any tab can open (session detail, live tail, tmux pane,
//! close confirmation).

use super::App;
use crate::config;
use crate::live_view::LiveView;
use crate::tmux_pane::TmuxPaneView;

#[derive(Clone, Debug, PartialEq)]
pub enum View {
    Grid,
    Popup,
    LiveTail,
    ConfirmClose,
    RenameSession,
    TmuxPane,
    FolderPicker,
    /// Model list for `N` on the Sessions tab: pick which Claude model the
    /// new session starts with.
    ModelPicker,
    /// Coding-agent picker for `A` on the Sessions tab. The selected agent
    /// becomes the default for subsequent new-session actions in this run.
    AgentPicker,
    /// Account picker for `R` on the Sessions tab: respawn the selected
    /// session on another subscription account, continuing from its
    /// transcript ([`crate::respawn`]).
    RespawnPicker,
    /// Fuzzy task selector for `L` on the Sessions tab: link (or unlink)
    /// the selected session to a personal-board or project task so the grid
    /// groups it under `project ▸ task`.
    TaskLinkPicker,
    /// Archive-wide session finder for `/` on the Sessions tab: fuzzy-search
    /// every transcript on disk (saved title, id, project, first message)
    /// and reopen the pick — attach when live, resume when not.
    SessionFinder,
    GhCreateInput,
    /// Centered single-line input for adding a task on the Tasks tab.
    TaskInput,
    /// Centered single-line input for editing the focused task's tags.
    TaskTags,
    /// Deliverable-kind list for `T` on the Tasks tab: pick the word the task
    /// router places the focused card by, or clear it back to router-chosen.
    TaskKindPicker,
    /// Task Info popup for the focused board card: prompt + attachments,
    /// with per-attachment copy/open/remove.
    TaskInfo,
    /// Centered single-line input for attaching to the focused board card:
    /// opens in note mode (the buffer becomes a typed `note` attachment),
    /// Tab flips it to file-path/URL mode.
    TaskAttachInput,
    /// Typing edits the Tasks-board filter live; the board renders
    /// underneath, already narrowed. Enter keeps the filter, Esc clears it.
    TaskFilter,
    /// Agents tab: runs, artifacts, log and settings of the focused agent.
    AgentDetail,
    /// Builds tab: the new-build form behind `n`.
    BuildForm,
    /// Builds tab: the selected build's output, following its end.
    BuildLog,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tab {
    Tasks,
    Sessions,
    Builds,
    Agents,
    Metrics,
}

impl Tab {
    pub fn label(&self) -> &'static str {
        match self {
            Tab::Tasks => "Tasks",
            Tab::Sessions => "Sessions",
            Tab::Builds => "Builds",
            Tab::Agents => "Agents",
            Tab::Metrics => "Metrics",
        }
    }
}

pub const TABS: &[Tab] = &[
    Tab::Tasks,
    Tab::Sessions,
    Tab::Builds,
    Tab::Agents,
    Tab::Metrics,
];

/// Tabs shown in the strip and reachable via ⇥, in [`TABS`] order.
pub fn visible_tabs() -> Vec<Tab> {
    TABS.iter()
        .copied()
        // Agents shows once `~/.cc-hub/agents/` exists (at startup) unless
        // config hides it; `cc-hub agent new` creates the dir.
        .filter(|t| match t {
            Tab::Agents => config::get().harness.show_tab && crate::harness::root_exists(),
            // Builds shows once a recipe exists to build with.
            Tab::Builds => !config::get().builds.recipes.is_empty(),
            _ => true,
        })
        .collect()
}

/// The terminal close staged behind [`View::ConfirmClose`].
#[derive(Clone, Debug)]
pub struct PendingClose {
    pub pid: u32,
    pub display: String,
}

impl App {
    pub fn set_tab(&mut self, tab: Tab) {
        // Entering the Tasks tab re-reads the board so edits from another
        // instance (or a hand-edited state.json) show up. The reload and the
        // re-floated live columns can both rearrange rows, so the cursor
        // follows its task by id rather than staying on a stale (col, row).
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
        self.step_tab(1);
    }

    pub fn cycle_tab_back(&mut self) {
        self.step_tab(-1);
    }

    /// Move `delta` tabs along the visible strip, wrapping at both ends.
    fn step_tab(&mut self, delta: isize) {
        let tabs = visible_tabs();
        let next = match tabs.iter().position(|t| *t == self.current_tab) {
            Some(i) => tabs[(i as isize + delta).rem_euclid(tabs.len() as isize) as usize],
            // The current tab can only be hidden by a config change, which
            // needs a restart; stay defensive and land on the first visible
            // tab rather than panicking.
            None => tabs.first().copied().unwrap_or(Tab::Sessions),
        };
        self.set_tab(next);
    }

    pub fn enter_tmux_pane(&mut self, view: TmuxPaneView) {
        self.tmux_pane = Some(view);
        self.view = View::TmuxPane;
    }

    pub fn close_tmux_pane(&mut self) {
        self.tmux_pane = None;
        self.view = self.view_under_overlay();
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
        self.view = self.view_under_overlay();
    }

    /// Where closing a transcript or pane lands: back in the agent detail
    /// it was opened from, else the grid.
    fn view_under_overlay(&self) -> View {
        if self.current_tab == Tab::Agents && self.harness.detail.is_some() {
            View::AgentDetail
        } else {
            View::Grid
        }
    }
}
