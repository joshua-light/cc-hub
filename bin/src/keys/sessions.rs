use cc_hub_lib::app::{App, Command, View};
use cc_hub_lib::focus;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Sessions- and Global-tab command mapping (Tasks lives in
/// [`super::tasks::map_tasks_command`]).
pub(super) fn map_sessions_command(
    app: &App,
    key: &KeyEvent,
    on_sessions: bool,
) -> Option<Command> {
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

pub(super) fn handle_confirm_close(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
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
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => {
            app.cancel_confirm_close();
        }
        _ => {}
    }
}

/// Session finder: printable keys (space included — titles and first
/// messages are prose) belong to the fuzzy filter; navigation is
/// arrows plus ctrl-j/k and ctrl-n/p. Enter is a command
/// ([`map_sessions_command`]).
pub(super) fn handle_finder(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.close_session_finder();
        }
        KeyCode::Down => app.session_finder_move(1),
        KeyCode::Up => app.session_finder_move(-1),
        KeyCode::Backspace => {
            if let Some(finder) = app.session_finder.as_mut() {
                finder.pop_filter();
            }
        }
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => match c {
            'j' | 'n' => app.session_finder_move(1),
            'k' | 'p' => app.session_finder_move(-1),
            _ => {}
        },
        KeyCode::Char(c) => {
            if let Some(finder) = app.session_finder.as_mut() {
                finder.push_filter(c);
            }
        }
        _ => {}
    }
}

pub(super) fn handle_rename(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.close_rename_session();
        }
        KeyCode::Backspace => {
            app.rename_buffer.pop();
        }
        KeyCode::Char(c) => {
            app.rename_buffer.push(c);
        }
        _ => {}
    }
}
