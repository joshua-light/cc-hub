//! TUI render entry: [`render`] is what the binary's draw loop calls. It lays
//! out the bands, draws the current tab's body, and dispatches the overlay
//! for the current view.
//!
//! - `chrome`: title bar and tab strip
//! - `status_bar`: key hints, status message and refresh age
//! - `popups`: overlays drawn over the body
//! - `common`, `palette`: helpers and colours shared across the UI
//! - `tasks`, `sessions`, `builds`, `agents`, `metrics`: one module per tab
//!   body
//! - `artifacts`: task attachment cards for the Task Info popup

pub mod agents;
pub mod artifacts;
pub mod builds;
mod chrome;
pub mod common;
pub mod metrics;
pub mod palette;
pub mod popups;
pub mod sessions;
mod status_bar;
pub mod tasks;

// Items consumed by bin/src/main.rs keep their `cc_hub_lib::ui::X` paths.
pub use common::build_usage_line;

use crate::app::{App, Tab, View};
use crate::config;
use chrome::{render_tab_strip, render_title_bar};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::Frame;
use status_bar::render_status_bar;

pub(crate) fn cell_height() -> u16 {
    config::get().ui.cell_height.max(1)
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Top-level vertical split: title bar, tab strip, body, status bar.
fn main_layout(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area)
}

pub fn render(frame: &mut Frame, app: &mut App) {
    app.update_grid_cols(frame.area().width);

    let chunks = main_layout(frame.area());

    render_title_bar(frame, chunks[0], app);
    render_tab_strip(frame, chunks[1], app);
    match app.current_tab {
        Tab::Tasks => tasks::render_tasks_body(frame, chunks[2], app),
        Tab::Sessions => sessions::render_sessions_body(frame, chunks[2], app),
        Tab::Builds => builds::render_builds_body(frame, chunks[2], app),
        Tab::Agents => agents::render_agents_body(frame, chunks[2], app),
        Tab::Metrics => metrics::render_metrics_body(frame, chunks[2], app),
    }
    render_status_bar(frame, chunks[3], app);

    match app.view {
        View::Popup => sessions::render_popup(frame, frame.area(), app),
        View::LiveTail => popups::render_live_tail(frame, frame.area(), app),
        View::ConfirmClose => popups::render_confirm_close(frame, frame.area(), app),
        View::ModelPicker => popups::render_model_picker(frame, frame.area(), app),
        View::AgentPicker => popups::render_agent_picker(frame, frame.area(), app),
        View::RespawnPicker => popups::render_respawn_picker(frame, frame.area(), app),
        View::TaskLinkPicker => popups::render_task_link_picker(frame, frame.area(), app),
        View::SessionFinder => popups::render_session_finder(frame, frame.area(), app),
        View::RenameSession => popups::render_rename_session(frame, frame.area(), app),
        View::TmuxPane => popups::render_tmux_pane(frame, frame.area(), app),
        View::FolderPicker => popups::render_folder_picker(frame, frame.area(), app),
        View::GhCreateInput => {
            popups::render_folder_picker(frame, frame.area(), app);
            popups::render_gh_create_input(frame, frame.area(), app);
        }
        View::TaskInput => popups::render_task_input(frame, frame.area(), app),
        View::TaskTags => popups::render_task_tags(frame, frame.area(), app),
        View::TaskKindPicker => popups::render_task_kind_picker(frame, frame.area(), app),
        View::TaskInfo => tasks::render_task_info(frame, frame.area(), app),
        View::TaskAttachInput => popups::render_task_attach_input(frame, frame.area(), app),
        // The filter bar lives inside the tasks body (already rendered
        // above), so filter-editing needs no overlay.
        View::TaskFilter => {}
        View::AgentDetail => agents::render_agent_detail(frame, frame.area(), app),
        View::BuildForm => builds::render_build_form(frame, frame.area(), app),
        View::BuildLog => builds::render_build_log(frame, frame.area(), app),
        View::Grid => {}
    }
}
