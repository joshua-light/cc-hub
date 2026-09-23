//! Key-event dispatch extracted from `run()`'s event loop.
//!
//! [`handle_key`] is the ~85-arm `(View, KeyCode)` match that used to live
//! inline in `run()`. It is a mechanical move — every arm preserves its exact
//! original behavior and ordering. Arms that used to `continue` the outer
//! loop return [`KeyOutcome::Continue`]; arms that fell through return
//! [`KeyOutcome::Proceed`]. Since `run()` switched to draining whole input
//! bursts before its per-pass scan drain, it treats both outcomes the same;
//! the variants survive as documentation of each arm's original intent.

use crate::ScanMsg;
use cc_hub_lib::app::{App, Command, View};
use cc_hub_lib::folder_picker::PickerMode;
use cc_hub_lib::{focus, live_view, models, platform};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

mod harness;
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
) -> Option<Command> {
    if let Some(cmd) = tasks::map_tasks_command(app, key, on_tasks) {
        return Some(cmd);
    }
    if let Some(cmd) = harness::map_harness_command(app, key, on_agents) {
        return Some(cmd);
    }
    map_sessions_command(app, key, on_sessions)
}

/// `f` on the Agents tab: open the transcript of the agent's tick — the one
/// in flight, or the last one it ran. A tick is headless, so there is no
/// pane to attach; the transcript is the whole of what there is to see, and
/// it tails live while the tick runs.
fn open_agent_transcript(app: &mut App) {
    let Some(agent) = app.harness.selected() else {
        return;
    };
    let name = agent.name.clone();
    let Some(path) = agent.transcript() else {
        app.set_status(format!("{}: no tick transcript yet", name));
        return;
    };
    let lv = live_view::LiveView::new(path.clone(), cc_hub_lib::agent::AgentKind::Claude);
    if lv.messages.is_empty() {
        app.set_status(format!("{}: {} is empty", name, path.display()));
    } else {
        app.enter_live_tail(lv);
    }
}

/// `o` on the Agents tab: open what the agent last pointed at — the
/// artifact, page or file in its newest note's `--ref` — with the OS
/// opener, the same way an attachment opens from a task card.
fn open_agent_ref(app: &mut App) {
    let Some(agent) = app.harness.selected() else {
        return;
    };
    let name = agent.name.clone();
    let Some(target) = agent.latest_ref().map(str::to_string) else {
        app.set_status(format!("{}: no note with a ref yet", name));
        return;
    };
    match crate::open_path_detached(&target) {
        Ok(()) => app.set_status(format!("opening {}", target)),
        Err(e) => app.set_status(format!("open failed: {}", e)),
    }
}

/// Sessions- and Global-tab command mapping (Tasks lives in
/// [`tasks::map_tasks_command`]).
fn map_sessions_command(app: &App, key: &KeyEvent, on_sessions: bool) -> Option<Command> {
    use cc_hub_lib::app::{GlobalCommand as G, SessionsCommand as S};
    if let Some(cmd) = map_agent_hotkey(app, key, on_sessions) {
        return Some(cmd);
    }
    let cmd = match (&app.view, key.code) {
        (View::Grid, KeyCode::Char('q')) => Command::Global(G::Quit),
        (View::Grid, KeyCode::Tab | KeyCode::Char('K')) => {
            Command::Global(G::CycleTab { back: false })
        }
        (View::Grid, KeyCode::BackTab | KeyCode::Char('J')) => {
            Command::Global(G::CycleTab { back: true })
        }
        (View::Grid, KeyCode::Char('m')) if on_sessions => Command::Global(G::SetTabMetrics),
        (View::Grid, KeyCode::Right | KeyCode::Char('l')) if on_sessions => {
            Command::Sessions(S::NavRight)
        }
        (View::Grid, KeyCode::Left | KeyCode::Char('h')) if on_sessions => {
            Command::Sessions(S::NavLeft)
        }
        (View::Grid, KeyCode::Down | KeyCode::Char('j')) if on_sessions => {
            Command::Sessions(S::NavDown)
        }
        (View::Grid, KeyCode::Up | KeyCode::Char('k')) if on_sessions => {
            Command::Sessions(S::NavUp)
        }
        (View::Grid, KeyCode::Char('i')) if on_sessions => Command::Sessions(S::OpenDetailPopup),
        (View::Grid, KeyCode::Char('H')) if on_sessions => Command::Sessions(S::ToggleShowInactive),
        (View::Grid, KeyCode::Char('v')) if on_sessions => Command::Sessions(S::ToggleLayout),
        (View::Grid, KeyCode::Char('f') | KeyCode::Enter) if on_sessions => {
            Command::Sessions(S::FocusSelected)
        }
        (View::Grid, KeyCode::Char('o')) if on_sessions => Command::Sessions(S::OpenShellHere),
        (View::Grid, KeyCode::Char('x')) if on_sessions => Command::Sessions(S::StageConfirmClose),
        (View::Grid, KeyCode::Char(' ')) if on_sessions => Command::Sessions(S::AckSelected),
        (View::Grid, KeyCode::Char('n')) if on_sessions => Command::Sessions(S::SpawnAgentHere),
        (View::Grid, KeyCode::Char('N')) if on_sessions => Command::Sessions(S::OpenModelPicker),
        (View::Grid, KeyCode::Char('A')) if on_sessions => Command::Sessions(S::OpenAgentPicker),
        (View::Grid, KeyCode::Char('R')) if on_sessions => Command::Sessions(S::OpenRespawnPicker),
        (View::Grid, KeyCode::Char('M')) if on_sessions => {
            Command::Sessions(S::OpenBookmarksPicker)
        }
        // Matches plain `p` and ⌘p alike — modifiers are deliberately not
        // checked here, so terminals that forward Cmd (kitty protocol)
        // land on the same arm.
        (View::Grid, KeyCode::Char('p')) if on_sessions => Command::Sessions(S::OpenPlacesPicker),
        (View::Grid, KeyCode::Char('L')) if on_sessions => Command::Sessions(S::OpenTaskLinkPicker),
        (View::Grid, KeyCode::Char('/')) if on_sessions => Command::Sessions(S::OpenSessionFinder),
        (View::SessionFinder, KeyCode::Enter) => Command::Sessions(S::ConfirmSessionFinder),
        (View::Grid, KeyCode::Char('r')) if on_sessions => Command::Sessions(S::OpenRenameSession),
        (View::RenameSession, KeyCode::Enter) => Command::Sessions(S::SubmitRename),
        _ => return None,
    };
    Some(cmd)
}

/// `[agents.<id>].hotkey` bindings on the Sessions grid. Checked before the
/// built-in table so a user who binds `N` to Claude gets Claude, not the
/// model picker — the config doc says custom hotkeys shadow built-ins.
/// Ctrl/Alt chords are left alone; Shift is what makes an uppercase char.
fn map_agent_hotkey(app: &App, key: &KeyEvent, on_sessions: bool) -> Option<Command> {
    use cc_hub_lib::app::SessionsCommand as S;
    if !on_sessions || app.view != View::Grid {
        return None;
    }
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    let agent_id = cc_hub_lib::config::get().agent_for_hotkey(c)?;
    Some(Command::Sessions(S::SpawnAgentHereWith { agent_id }))
}

/// Dispatch a single key press. `spawn_metrics` is the run()-local closure
/// that kicks the background metrics scan; it is threaded through as a
/// callback so the three arms that need it keep their exact behavior without
/// pulling the closure (which captures `run()` locals) out of `run()`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_key(
    app: &mut App,
    key: KeyEvent,
    terminal: &crate::Term,
    scan_tx_main: &mpsc::Sender<ScanMsg>,
    detail_tx: &mpsc::Sender<String>,
    spawn_metrics: &impl Fn(),
    on_sessions: bool,
    on_metrics: bool,
    on_tasks: bool,
    on_agents: bool,
) -> KeyOutcome {
    if let Some(cmd) = map_command(app, &key, on_sessions, on_tasks, on_agents) {
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
    match (&app.view, key.code) {
        (View::Grid, KeyCode::Down | KeyCode::Char('j')) if on_metrics => {
            app.metrics_nav_down();
        }
        (View::Grid, KeyCode::Up | KeyCode::Char('k')) if on_metrics => {
            app.metrics_nav_up();
        }
        (View::Grid, KeyCode::Enter) if on_metrics => {
            if let Some(row) = app.selected_metrics_session().cloned() {
                let agent_kind = if platform::paths::pi_sessions_dir()
                    .as_ref()
                    .is_some_and(|dir| row.jsonl_path.starts_with(dir))
                {
                    cc_hub_lib::agent::AgentKind::Pi
                } else {
                    cc_hub_lib::agent::AgentKind::Claude
                };
                let lv = live_view::LiveView::review(
                    row.jsonl_path.clone(),
                    agent_kind,
                    row.peak_timestamp_ms,
                );
                if lv.messages.is_empty() {
                    app.set_status(format!(
                        "can't open {}: {} missing or empty",
                        models::short_sid(&row.session_id),
                        row.jsonl_path.display()
                    ));
                } else {
                    app.enter_live_tail(lv);
                }
            }
        }
        (View::Grid | View::AgentDetail, KeyCode::Char('f')) if on_agents => {
            open_agent_transcript(app);
        }
        (View::Grid | View::AgentDetail, KeyCode::Char('o')) if on_agents => {
            open_agent_ref(app);
        }
        (View::Grid, KeyCode::Char('r')) if on_metrics => {
            app.metrics.analysis = None;
            spawn_metrics();
        }
        (View::TmuxPane, KeyCode::F(1)) => {
            app.close_tmux_pane();
        }
        (View::TmuxPane, KeyCode::Char(c))
            if (c == 'v' || c == 'V')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && key.modifiers.contains(KeyModifiers::SHIFT) =>
        {
            let status = match cc_hub_lib::clipboard::paste() {
                Ok(text) if text.is_empty() => Some("clipboard empty".to_string()),
                Ok(text) => match app.tmux_pane.as_ref() {
                    Some(pane) => pane
                        .paste_text(&text)
                        .err()
                        .map(|e| format!("paste failed: {}", e)),
                    None => None,
                },
                Err(e) => Some(format!("paste failed: {}", e)),
            };
            if let Some(msg) = status {
                app.set_status(msg);
            }
        }
        (View::TmuxPane, _) => {
            if let Some(pane) = app.tmux_pane.as_mut() {
                pane.send_key(key);
            }
        }
        (View::ConfirmClose, KeyCode::Char('y') | KeyCode::Char('Y')) => {
            if let Some(pending) = app.take_pending_close() {
                let ok = focus::close_window(pending.pid);
                let msg = if ok {
                    format!("closed {}", pending.display)
                } else {
                    format!("failed to close {}", pending.display)
                };
                app.set_status(msg);
            }
        }
        (
            View::ConfirmClose,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q'),
        ) => {
            app.cancel_confirm_close();
        }
        // Places mode (task assign): printable keys type into the fuzzy
        // filter, so the generic picker bindings below (j/k/q/m/. etc.)
        // must not fire. Navigation is arrows plus ctrl-j/k and ctrl-n/p.
        (View::FolderPicker, code)
            if app
                .folder_picker
                .as_ref()
                .is_some_and(|p| p.mode == PickerMode::Places) =>
        {
            match code {
                KeyCode::Esc => app.close_folder_picker(),
                KeyCode::Enter | KeyCode::Char(' ') => {
                    // Don't let an empty match list cancel the picker —
                    // pick_from_folder_picker closes on no selection.
                    if app
                        .folder_picker
                        .as_ref()
                        .is_some_and(|p| p.selected_path().is_some())
                    {
                        crate::pick_from_folder_picker(app);
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
        }
        (View::FolderPicker, KeyCode::Esc | KeyCode::Char('q')) => {
            app.close_folder_picker();
        }
        // Browse → back to the places list.
        (View::FolderPicker, KeyCode::Tab) => {
            app.toggle_places_picker_mode();
        }
        (View::FolderPicker, KeyCode::Down | KeyCode::Char('j')) => {
            if let Some(p) = app.folder_picker.as_mut() {
                p.move_down();
            }
        }
        (View::FolderPicker, KeyCode::Up | KeyCode::Char('k')) => {
            if let Some(p) = app.folder_picker.as_mut() {
                p.move_up();
            }
        }
        (View::FolderPicker, KeyCode::Char('m')) => match app.toggle_selected_bookmark() {
            Some((true, path)) => app.set_status(format!("bookmarked {}", path)),
            Some((false, path)) => app.set_status(format!("unbookmarked {}", path)),
            None => app.set_status("no folder selected".into()),
        },
        (View::FolderPicker, KeyCode::Enter | KeyCode::Right | KeyCode::Char('l')) => {
            let bookmarks_mode = app
                .folder_picker
                .as_ref()
                .is_some_and(|p| p.mode == PickerMode::Bookmarks);
            if bookmarks_mode {
                crate::pick_from_folder_picker(app);
            } else if let Some(p) = app.folder_picker.as_mut() {
                p.descend();
            }
        }
        (View::FolderPicker, KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h')) => {
            if let Some(p) = app.folder_picker.as_mut() {
                p.ascend();
            }
        }
        (View::FolderPicker, KeyCode::Char(' ')) => {
            crate::pick_from_folder_picker(app);
        }
        (View::FolderPicker, KeyCode::Char('.')) => {
            // Bookmarks mode has no meaningful "current dir" —
            // the entries are absolute paths from disk — so
            // collapse `.` into the same action as space/Enter.
            let bookmarks_mode = app
                .folder_picker
                .as_ref()
                .is_some_and(|p| p.mode == PickerMode::Bookmarks);
            if bookmarks_mode {
                crate::pick_from_folder_picker(app);
            } else {
                let cwd = app
                    .folder_picker
                    .as_ref()
                    .map(|p| p.current_dir.display().to_string());
                if let Some(cwd) = cwd {
                    crate::dispatch_picked_cwd(app, &cwd);
                } else {
                    app.close_folder_picker();
                }
            }
        }
        (View::FolderPicker, KeyCode::Char('c')) => {
            if !app
                .folder_picker
                .as_ref()
                .is_some_and(|p| p.mode == PickerMode::Bookmarks)
            {
                app.enter_gh_create_input(false);
            }
        }
        (View::FolderPicker, KeyCode::Char('C')) => {
            if !app
                .folder_picker
                .as_ref()
                .is_some_and(|p| p.mode == PickerMode::Bookmarks)
            {
                app.enter_gh_create_input(true);
            }
        }
        (View::GhCreateInput, KeyCode::Esc) => {
            app.close_gh_create_input();
        }
        (View::GhCreateInput, KeyCode::Tab) => {
            if let Some(input) = app.gh_create_input.as_mut() {
                input.private = !input.private;
            }
        }
        (View::GhCreateInput, KeyCode::Backspace) => {
            if let Some(input) = app.gh_create_input.as_mut() {
                input.name.pop();
            }
        }
        (View::GhCreateInput, KeyCode::Char(c)) => {
            if let Some(input) = app.gh_create_input.as_mut() {
                input.name.push(c);
            }
        }
        (View::GhCreateInput, KeyCode::Enter) => {
            let name_empty = app
                .gh_create_input
                .as_ref()
                .is_none_or(|i| i.name.trim().is_empty());
            if name_empty {
                app.set_status("repo name cannot be empty".into());
                return KeyOutcome::Continue;
            }
            if let Some((cwd, name, private)) = app.submit_gh_create_input() {
                let trimmed = name.trim().to_string();
                let tx = scan_tx_main.clone();
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
        // Like the places picker, printable keys belong to the fuzzy filter;
        // use arrows or ctrl-j/k/n/p to navigate without stealing letters.
        (View::ModelPicker, code) => match code {
            KeyCode::Esc => app.close_model_picker(),
            KeyCode::Enter | KeyCode::Char(' ') => app.spawn_from_model_picker(),
            KeyCode::Tab => app.cycle_model_picker_agent(),
            KeyCode::Down => app.model_picker_move(1),
            KeyCode::Up => app.model_picker_move(-1),
            KeyCode::Backspace => {
                if let Some(picker) = app.model_picker.as_mut() {
                    picker.pop_filter();
                }
            }
            KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => match c {
                'j' | 'n' => app.model_picker_move(1),
                'k' | 'p' => app.model_picker_move(-1),
                _ => {}
            },
            KeyCode::Char(c) => {
                if let Some(picker) = app.model_picker.as_mut() {
                    picker.push_filter(c);
                }
            }
            _ => {}
        },
        (View::AgentPicker, code) => match code {
            KeyCode::Esc => app.close_agent_picker(),
            KeyCode::Enter | KeyCode::Char(' ') => app.confirm_default_session_agent(),
            KeyCode::Down | KeyCode::Char('j') => app.agent_picker_move(1),
            KeyCode::Up | KeyCode::Char('k') => app.agent_picker_move(-1),
            _ => {}
        },
        (View::RespawnPicker, code) => match code {
            KeyCode::Esc => app.close_respawn_picker(),
            KeyCode::Enter | KeyCode::Char(' ') => app.confirm_respawn_picker(),
            KeyCode::Down | KeyCode::Char('j') => app.respawn_picker_move(1),
            KeyCode::Up | KeyCode::Char('k') => app.respawn_picker_move(-1),
            _ => {}
        },
        (View::TaskKindPicker, code) => match code {
            KeyCode::Esc => app.close_task_kind_picker(),
            KeyCode::Enter | KeyCode::Char(' ') => {
                app.confirm_task_kind();
            }
            KeyCode::Down | KeyCode::Char('j') => app.task_kind_picker_move(1),
            KeyCode::Up | KeyCode::Char('k') => app.task_kind_picker_move(-1),
            _ => {}
        },
        // Same interaction model as the model picker: printable keys belong
        // to the fuzzy filter; arrows or ctrl-j/k/n/p navigate.
        (View::TaskLinkPicker, code) => match code {
            KeyCode::Esc => app.close_task_link_picker(),
            KeyCode::Enter | KeyCode::Char(' ') => app.confirm_task_link_picker(),
            KeyCode::Down => app.task_link_picker_move(1),
            KeyCode::Up => app.task_link_picker_move(-1),
            KeyCode::Backspace => {
                if let Some(picker) = app.task_link_picker.as_mut() {
                    picker.pop_filter();
                }
            }
            KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => match c {
                'j' | 'n' => app.task_link_picker_move(1),
                'k' | 'p' => app.task_link_picker_move(-1),
                _ => {}
            },
            KeyCode::Char(c) => {
                if let Some(picker) = app.task_link_picker.as_mut() {
                    picker.push_filter(c);
                }
            }
            _ => {}
        },
        // Session finder: printable keys (space included — titles and first
        // messages are prose) belong to the fuzzy filter; navigation is
        // arrows plus ctrl-j/k and ctrl-n/p. Enter is a command above.
        (View::SessionFinder, KeyCode::Esc) => {
            app.close_session_finder();
        }
        (View::SessionFinder, KeyCode::Down) => app.session_finder_move(1),
        (View::SessionFinder, KeyCode::Up) => app.session_finder_move(-1),
        (View::SessionFinder, KeyCode::Backspace) => {
            if let Some(finder) = app.session_finder.as_mut() {
                finder.pop_filter();
            }
        }
        (View::SessionFinder, KeyCode::Char(c))
            if key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            match c {
                'j' | 'n' => app.session_finder_move(1),
                'k' | 'p' => app.session_finder_move(-1),
                _ => {}
            }
        }
        (View::SessionFinder, KeyCode::Char(c)) => {
            if let Some(finder) = app.session_finder.as_mut() {
                finder.push_filter(c);
            }
        }
        (View::RenameSession, KeyCode::Esc) => {
            app.close_rename_session();
        }
        (View::RenameSession, KeyCode::Backspace) => {
            app.rename_buffer.pop();
        }
        (View::RenameSession, KeyCode::Char(c)) => {
            app.rename_buffer.push(c);
        }
        // Popup navigation
        (View::Popup, KeyCode::Esc | KeyCode::Char('q')) => app.close_popup(),
        (View::Popup, KeyCode::Down | KeyCode::Char('j')) => app.scroll_down(),
        (View::Popup, KeyCode::Up | KeyCode::Char('k')) => app.scroll_up(),
        // Live tail view
        (View::LiveTail, KeyCode::Esc | KeyCode::Char('q')) => {
            app.close_live_tail();
        }
        (View::LiveTail, KeyCode::Down | KeyCode::Char('j')) => {
            if let Some(ref mut lv) = app.live_view {
                lv.scroll_down();
            }
        }
        (View::LiveTail, KeyCode::Up | KeyCode::Char('k')) => {
            if let Some(ref mut lv) = app.live_view {
                lv.scroll_up();
            }
        }
        (View::LiveTail, KeyCode::Char('G')) => {
            if let Some(ref mut lv) = app.live_view {
                lv.scroll_bottom();
            }
        }
        _ => {}
    }
    KeyOutcome::Proceed
}
