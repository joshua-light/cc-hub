//! Interpreter for the lib-side [`Effect`] contract.
//!
//! [`cc_hub_lib::app::App::execute`] performs every in-process consequence of
//! a command and returns the effects that need bin-owned machinery: the
//! terminal (pane sizing), the `run()` channels, or the window manager. This
//! module is deliberately a thin IO shim — decisions and status messaging
//! belong in `App::execute`, with two exceptions that *are* IO outcomes
//! (pane/shell attach failures, window-reattach results), whose status
//! strings preserve the original inline arms verbatim.

use crate::scan_msg::ScanMsg;
use crate::term::Term;
use cc_hub_lib::app::{App, Effect};
use cc_hub_lib::{focus, spawn, tmux_pane};
use std::io;
use tokio::sync::mpsc;

pub(crate) async fn apply_effect(
    app: &mut App,
    effect: Effect,
    terminal: &Term,
    scan_tx: &mpsc::Sender<ScanMsg>,
    detail_tx: &mpsc::Sender<String>,
) {
    match effect {
        Effect::RequestSessionDetail { session_id } => {
            let _ = detail_tx.send(session_id).await;
        }
        Effect::SpawnMetricsScan => crate::workers::spawn_metrics(scan_tx.clone()),
        Effect::BuildSessionIndex => {
            let tx = scan_tx.clone();
            tokio::spawn(async move {
                let index = tokio::task::spawn_blocking(cc_hub_lib::sessions::index::scan)
                    .await
                    .unwrap_or_default();
                let _ = tx.send(ScanMsg::SessionIndex(index)).await;
            });
        }
        Effect::OpenTmuxPane { tmux, owned } => {
            let (cols, rows) = popup_pane_size(terminal);
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
            let (cols, rows) = popup_pane_size(terminal);
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
        Effect::OpenExternal { target } => {
            if let Err(e) = open_path_detached(&target) {
                app.set_status(format!("open failed: {}", e));
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

/// Size for a popup tmux pane: terminal minus a margin, with floor. The
/// renderer re-resizes on first draw, so a rough starting size is fine.
fn popup_pane_size(terminal: &Term) -> (u16, u16) {
    terminal
        .size()
        .map(|s| {
            (
                s.width.saturating_sub(6).max(20),
                s.height.saturating_sub(6).max(10),
            )
        })
        .unwrap_or((120, 30))
}

/// Spawn the OS-default opener for `path` and detach immediately. URLs work
/// the same as files because `xdg-open` / `open` / `cmd start` all dispatch
/// by scheme. Output is dropped — we don't surface stderr because most
/// failures here mean "no DE installed", which the status bar already
/// reports via the `Err` path of [`std::process::Command::spawn`].
pub(crate) fn open_path_detached(path: &str) -> io::Result<()> {
    use std::process::{Command, Stdio};
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(path);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/c", "start", "", path]);
        c
    };
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(path);
        c
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}
