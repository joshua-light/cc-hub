use cc_hub_lib::app::{App, Command, HarnessCommand as H, View};
use crossterm::event::{KeyCode, KeyEvent};

/// Agents-tab and agent-detail key mapping.
pub(super) fn map_harness_command(app: &App, key: &KeyEvent, on_agents: bool) -> Option<Command> {
    let cmd = match (&app.view, key.code) {
        (View::Grid, KeyCode::Down | KeyCode::Char('j')) if on_agents => H::NavDown,
        (View::Grid, KeyCode::Up | KeyCode::Char('k')) if on_agents => H::NavUp,
        (View::Grid, KeyCode::Right | KeyCode::Char('l')) if on_agents => H::NavRight,
        (View::Grid, KeyCode::Left | KeyCode::Char('h')) if on_agents => H::NavLeft,
        (View::Grid, KeyCode::Enter | KeyCode::Char('i')) if on_agents => H::OpenDetail,
        (View::Grid, KeyCode::Char('p')) if on_agents => H::Poke,
        (View::Grid, KeyCode::Char(' ')) if on_agents => H::TogglePause,
        (View::Grid, KeyCode::Char('R')) if on_agents => H::Reset,
        (View::AgentDetail, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) => H::CloseDetail,
        (View::AgentDetail, KeyCode::Down | KeyCode::Char('j')) => H::DetailScrollDown,
        (View::AgentDetail, KeyCode::Up | KeyCode::Char('k')) => H::DetailScrollUp,
        (View::AgentDetail, KeyCode::Char('p')) => H::Poke,
        (View::AgentDetail, KeyCode::Char(' ')) => H::TogglePause,
        (View::AgentDetail, KeyCode::Char('R')) => H::Reset,
        _ => return None,
    };
    Some(Command::Harness(cmd))
}
