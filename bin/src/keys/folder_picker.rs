// Clippy wants each arm's lone `if` folded into a match guard. A failing
// guard falls through to later arms (Places-mode Space would then type into
// the filter), so the nested `if`s stay.
#![allow(clippy::collapsible_match)]

use crate::scan_msg::ScanMsg;
use cc_hub_lib::app::App;
use cc_hub_lib::folder_picker::PickerMode;
use cc_hub_lib::spawn;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

pub(super) fn handle(app: &mut App, key: KeyEvent) {
    // Places mode (task assign): printable keys type into the fuzzy
    // filter, so the generic picker bindings below (j/k/q/m/. etc.)
    // must not fire. Navigation is arrows plus ctrl-j/k and ctrl-n/p.
    if app
        .folder_picker
        .as_ref()
        .is_some_and(|p| p.mode == PickerMode::Places)
    {
        match key.code {
            KeyCode::Esc => app.close_folder_picker(),
            KeyCode::Enter | KeyCode::Char(' ') => {
                // Don't let an empty match list cancel the picker —
                // pick_from_folder_picker closes on no selection.
                if app
                    .folder_picker
                    .as_ref()
                    .is_some_and(|p| p.selected_path().is_some())
                {
                    pick_from_folder_picker(app);
                }
            }
            KeyCode::Tab => app.toggle_places_picker_mode(),
            KeyCode::Down => {
                if let Some(p) = app.folder_picker.as_mut() {
                    p.move_down();
                }
            }
            KeyCode::Up => {
                if let Some(p) = app.folder_picker.as_mut() {
                    p.move_up();
                }
            }
            KeyCode::Backspace => {
                if let Some(p) = app.folder_picker.as_mut() {
                    p.pop_filter();
                }
            }
            KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(p) = app.folder_picker.as_mut() {
                    match c {
                        'j' | 'n' => p.move_down(),
                        'k' | 'p' => p.move_up(),
                        _ => {}
                    }
                }
            }
            KeyCode::Char(c) => {
                if let Some(p) = app.folder_picker.as_mut() {
                    p.push_filter(c);
                }
            }
            _ => {}
        }
        return;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.close_folder_picker();
        }
        // Browse → back to the places list.
        KeyCode::Tab => {
            app.toggle_places_picker_mode();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(p) = app.folder_picker.as_mut() {
                p.move_down();
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(p) = app.folder_picker.as_mut() {
                p.move_up();
            }
        }
        KeyCode::Char('m') => match app.toggle_selected_bookmark() {
            Some((true, path)) => app.set_status(format!("bookmarked {}", path)),
            Some((false, path)) => app.set_status(format!("unbookmarked {}", path)),
            None => app.set_status("no folder selected".into()),
        },
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
            if in_bookmarks(app) {
                pick_from_folder_picker(app);
            } else if let Some(p) = app.folder_picker.as_mut() {
                p.descend();
            }
        }
        KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
            if let Some(p) = app.folder_picker.as_mut() {
                p.ascend();
            }
        }
        KeyCode::Char(' ') => {
            pick_from_folder_picker(app);
        }
        KeyCode::Char('.') => {
            // Bookmarks mode has no meaningful "current dir" —
            // the entries are absolute paths from disk — so
            // collapse `.` into the same action as space/Enter.
            if in_bookmarks(app) {
                pick_from_folder_picker(app);
            } else {
                let cwd = app
                    .folder_picker
                    .as_ref()
                    .map(|p| p.current_dir.display().to_string());
                if let Some(cwd) = cwd {
                    dispatch_picked_cwd(app, &cwd);
                } else {
                    app.close_folder_picker();
                }
            }
        }
        KeyCode::Char(c @ ('c' | 'C')) => {
            if !in_bookmarks(app) {
                app.enter_gh_create_input(c == 'C');
            }
        }
        _ => {}
    }
}

pub(super) fn handle_gh_create(app: &mut App, key: KeyEvent, scan_tx: &mpsc::Sender<ScanMsg>) {
    match key.code {
        KeyCode::Esc => {
            app.close_gh_create_input();
        }
        KeyCode::Tab => {
            if let Some(input) = app.gh_create_input.as_mut() {
                input.private = !input.private;
            }
        }
        KeyCode::Backspace => {
            if let Some(input) = app.gh_create_input.as_mut() {
                input.name.pop();
            }
        }
        KeyCode::Char(c) => {
            if let Some(input) = app.gh_create_input.as_mut() {
                input.name.push(c);
            }
        }
        KeyCode::Enter => {
            let name_empty = app
                .gh_create_input
                .as_ref()
                .is_none_or(|i| i.name.trim().is_empty());
            if name_empty {
                app.set_status("repo name cannot be empty".into());
                return;
            }
            if let Some((cwd, name, private)) = app.submit_gh_create_input() {
                let trimmed = name.trim().to_string();
                let tx = scan_tx.clone();
                let name_for_msg = trimmed.clone();
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        cc_hub_lib::gh::create_repo(&cwd, &trimmed, private)
                    })
                    .await
                    .unwrap_or_else(|e| Err(format!("task panicked: {}", e)));
                    let _ = tx
                        .send(ScanMsg::GhCreateDone {
                            name: name_for_msg,
                            result,
                        })
                        .await;
                });
                app.set_status(format!(
                    "creating {} repo {}…",
                    if private { "private" } else { "public" },
                    name
                ));
            }
        }
        _ => {}
    }
}

fn in_bookmarks(app: &App) -> bool {
    app.folder_picker
        .as_ref()
        .is_some_and(|p| p.mode == PickerMode::Bookmarks)
}

/// Act on a folder-picker pick at `cwd`: assign the pending board task
/// there, or spawn a session.
fn dispatch_picked_cwd(app: &mut App, cwd: &str) {
    if app.tasks.pending_assign.is_some() {
        let status = app.assign_task_agent(cwd);
        app.set_status(status);
    } else {
        app.close_folder_picker();
        let agent_id = app.default_session_agent_id().to_string();
        let status = match spawn::spawn_agent_session(&agent_id, cwd, None, None, None, false) {
            Ok(name) => {
                let status = format!("started {} [{}]", agent_id, name);
                app.watch_spawn(name, agent_id, cwd.to_string());
                status
            }
            Err(e) => format!("spawn failed: {}", e),
        };
        app.set_status(status);
    }
}

/// Resolve the highlighted picker entry and dispatch it via
/// [`dispatch_picked_cwd`]. Works in both Browse and Bookmarks mode since
/// the lookup goes through `FolderPicker::selected_path`. No-ops by
/// closing the picker when nothing is selected.
fn pick_from_folder_picker(app: &mut App) {
    let cwd = app
        .folder_picker
        .as_ref()
        .and_then(|p| p.selected_path())
        .map(|p| p.display().to_string());
    match cwd {
        Some(cwd) => dispatch_picked_cwd(app, &cwd),
        None => app.close_folder_picker(),
    }
}
