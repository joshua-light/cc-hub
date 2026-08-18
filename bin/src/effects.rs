//! Interpreter for the lib-side [`Effect`] contract.
//!
//! [`cc_hub_lib::app::App::execute`] performs every in-process consequence of
//! a command and returns the effects that need bin-owned machinery: the
//! terminal (pane sizing), the `run()` channels, or the window manager. This
//! module is deliberately a thin IO shim — decisions and status messaging
//! belong in `App::execute`, with two exceptions that *are* IO outcomes
//! (pane/shell attach failures, window-reattach results), whose status
//! strings preserve the original inline arms verbatim.

use cc_hub_lib::app::{App, Effect};
use cc_hub_lib::{focus, spawn, tmux_pane};
use tokio::sync::mpsc;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_effect(
    app: &mut App,
    effect: Effect,
    terminal: &crate::Term,
    scan_tx: &mpsc::Sender<crate::ScanMsg>,
    detail_tx: &mpsc::Sender<String>,
    state_debug_tx: &mpsc::Sender<String>,
    spawn_metrics: &impl Fn(),
) {
    match effect {
        Effect::RequestSessionDetail { session_id } => {
            let _ = detail_tx.send(session_id).await;
        }
        Effect::RequestStateDebug { session_id } => {
            let _ = state_debug_tx.send(session_id).await;
        }
        Effect::SpawnMetricsScan => spawn_metrics(),
        Effect::BuildSessionIndex => {
            let tx = scan_tx.clone();
            tokio::spawn(async move {
                let index = tokio::task::spawn_blocking(cc_hub_lib::session_index::scan)
                    .await
                    .unwrap_or_default();
                let _ = tx.send(crate::ScanMsg::SessionIndex(index)).await;
            });
        }
        Effect::OpenTmuxPane { tmux, owned } => {
            let (cols, rows) = crate::popup_pane_size(terminal);
            let pane = if owned {
                tmux_pane::TmuxPaneView::spawn_owned(&tmux, rows, cols)
            } else {
                tmux_pane::TmuxPaneView::spawn(&tmux, rows, cols)
            };
            match pane {
                Ok(pane) => app.enter_tmux_pane(pane),
                Err(e) => app.set_status(format!("tmux attach failed: {}", e)),
            }
        }
        Effect::OpenShell { cwd } => {
            let (cols, rows) = crate::popup_pane_size(terminal);
            match spawn::spawn_shell_tmux_session(&cwd) {
                Ok(tmux_name) => match tmux_pane::TmuxPaneView::spawn_owned(&tmux_name, rows, cols)
                {
                    Ok(pane) => app.enter_tmux_pane(pane),
                    Err(e) => app.set_status(format!("shell attach failed: {}", e)),
                },
                Err(e) => {
                    app.set_status(format!("shell spawn failed: {}", e));
                }
            }
        }
        Effect::FocusWindow { pid, cwd } => match focus::focus_window(pid) {
            focus::FocusOutcome::Focused => {}
            focus::FocusOutcome::NeedsReattach(name) => {
                let msg = match spawn::attach_tmux_session(&name, &cwd) {
                    Ok(_) => format!("reattached terminal to {}", name),
                    Err(e) => format!("reattach failed: {}", e),
                };
                app.set_status(msg);
            }
            focus::FocusOutcome::Failed(msg) => {
                app.set_status(msg);
            }
        },
    }
}
