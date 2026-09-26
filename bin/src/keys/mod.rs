//! Key-event dispatch extracted from `run()`'s event loop.
//!
//! [`handle_key`] is the ~85-arm `(View, KeyCode)` match that used to live
//! inline in `run()`. It is a mechanical move — every arm preserves its exact
//! original behavior and ordering. Arms that used to `continue` the outer
//! loop return [`KeyOutcome::Continue`]; arms that fell through return
//! [`KeyOutcome::Proceed`]. Since `run()` switched to draining whole input
//! bursts before its per-pass scan drain, it treats both outcomes the same;
//! the variants survive as documentation of each arm's original intent.
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
use cc_hub_lib::app::{App, Command, View};
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

/// Historically: whether `run()` should skip the rest of its loop iteration
/// (the scan drain + pending-dispatch poll). `Continue` mirrors the
/// `continue` statements the match arms used when they lived inline. `run()`
/// now handles both variants identically — kept because the distinction
/// still documents which arms fully consumed their key.
pub(crate) enum KeyOutcome {
    Continue,
    Proceed,
}

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

/// Dispatch a single key press. `spawn_metrics` is the run()-local closure
/// that kicks the background metrics scan; it is threaded through as a
/// callback so the three arms that need it keep their exact behavior without
/// pulling the closure (which captures `run()` locals) out of `run()`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_key(
    app: &mut App,
    key: KeyEvent,
    terminal: &Term,
    scan_tx_main: &mpsc::Sender<ScanMsg>,
    detail_tx: &mpsc::Sender<String>,
    spawn_metrics: &impl Fn(),
    on_sessions: bool,
    on_metrics: bool,
    on_tasks: bool,
    on_agents: bool,
    on_builds: bool,
) -> KeyOutcome {
    if let Some(cmd) = map_command(app, &key, on_sessions, on_tasks, on_agents, on_builds) {
        for effect in app.execute(cmd) {
            crate::effects::apply_effect(
                app,
                effect,
                terminal,
                scan_tx_main,
                detail_tx,
                spawn_metrics,
            )
            .await;
        }
        return KeyOutcome::Continue;
    }
    if tasks::handle(app, key) {
        return KeyOutcome::Continue;
    }
    // Every view's arms are disjoint, so routing on the view first keeps
    // each view's own arm order.
    match app.view {
        View::Grid if on_metrics => metrics::handle(app, key, spawn_metrics),
        View::TmuxPane => pane::handle(app, key),
        View::ConfirmClose => sessions::handle_confirm_close(app, key),
        View::FolderPicker => folder_picker::handle(app, key),
        View::GhCreateInput => folder_picker::handle_gh_create(app, key, scan_tx_main),
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
    KeyOutcome::Proceed
}
