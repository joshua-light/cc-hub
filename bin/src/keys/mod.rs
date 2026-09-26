//! Key dispatch for the TUI. A key first maps onto a lib [`Command`]
//! ([`map_command`] -> `App::execute` -> [`crate::effects`]); keys no command
//! covers go to the current view's bin-side handler.
//!
//! - `sessions`: Sessions-tab commands, close confirmation, session finder,
//!   rename.
//! - `tasks`, `harness`, `builds`: the Tasks, Agents and Builds tabs.
//! - `folder_picker`: the folder picker and its `gh repo create` input.
//! - `pickers`: model, agent, respawn, task-kind and task-link pickers.
//! - `metrics`: the Metrics tab.
//! - `pane`: the embedded tmux pane.

use crate::scan_msg::ScanMsg;
use crate::term::Term;
use cc_hub_lib::app::{App, Command, Tab, View};
use crossterm::event::{KeyCode, KeyEvent};
use tokio::sync::mpsc;

mod builds;
mod folder_picker;
mod harness;
mod metrics;
mod pane;
mod pickers;
mod sessions;
mod tasks;

/// Map a key press onto a [`Command`] when a converted arm covers it.
///
/// Guards mirror the original match arms exactly; anything returning `None`
/// falls through to the legacy match below.
pub(super) fn map_command(
    app: &App,
    key: &KeyEvent,
    on_sessions: bool,
    on_tasks: bool,
    on_agents: bool,
    on_builds: bool,
) -> Option<Command> {
    if let Some(cmd) = tasks::map_tasks_command(app, key, on_tasks) {
        return Some(cmd);
    }
    if let Some(cmd) = harness::map_harness_command(app, key, on_agents) {
        return Some(cmd);
    }
    if let Some(cmd) = builds::map_builds_command(app, key, on_builds) {
        return Some(cmd);
    }
    sessions::map_sessions_command(app, key, on_sessions)
}

/// Dispatch a single key press: commands first, then the Tasks modal
/// editors, then the view's own bin-side handler.
pub(crate) async fn handle_key(
    app: &mut App,
    key: KeyEvent,
    terminal: &Term,
    scan_tx: &mpsc::Sender<ScanMsg>,
    detail_tx: &mpsc::Sender<String>,
) {
    let on_tab = |tab: Tab| app.view == View::Grid && app.current_tab == tab;
    let on_sessions = on_tab(Tab::Sessions);
    let on_metrics = on_tab(Tab::Metrics);
    let on_tasks = on_tab(Tab::Tasks);
    let on_agents = on_tab(Tab::Agents);
    let on_builds = on_tab(Tab::Builds);
    if let Some(cmd) = map_command(app, &key, on_sessions, on_tasks, on_agents, on_builds) {
        for effect in app.execute(cmd) {
            crate::effects::apply_effect(app, effect, terminal, scan_tx, detail_tx).await;
        }
        return;
    }
    if tasks::handle(app, key) {
        return;
    }
    // Every view's arms are disjoint, so routing on the view first keeps
    // each view's own arm order.
    match app.view {
        View::Grid if on_metrics => metrics::handle(app, key, scan_tx),
        View::TmuxPane => pane::handle(app, key),
        View::ConfirmClose => sessions::handle_confirm_close(app, key),
        View::FolderPicker => folder_picker::handle(app, key),
        View::GhCreateInput => folder_picker::handle_gh_create(app, key, scan_tx),
        View::ModelPicker => pickers::model(app, key),
        View::AgentPicker => pickers::agent(app, key),
        View::RespawnPicker => pickers::respawn(app, key),
        View::TaskKindPicker => pickers::task_kind(app, key),
        View::TaskLinkPicker => pickers::task_link(app, key),
        View::SessionFinder => sessions::handle_finder(app, key),
        View::RenameSession => sessions::handle_rename(app, key),
        View::Popup => match key.code {
            KeyCode::Esc | KeyCode::Char('q') => app.close_popup(),
            KeyCode::Down | KeyCode::Char('j') => app.scroll_down(),
            KeyCode::Up | KeyCode::Char('k') => app.scroll_up(),
            _ => {}
        },
        View::LiveTail => match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                app.close_live_tail();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(ref mut lv) = app.live_view {
                    lv.scroll_down();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(ref mut lv) = app.live_view {
                    lv.scroll_up();
                }
            }
            KeyCode::Char('G') => {
                if let Some(ref mut lv) = app.live_view {
                    lv.scroll_bottom();
                }
            }
            _ => {}
        },
        _ => {}
    }
}
