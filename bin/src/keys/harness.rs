use cc_hub_lib::app::{App, Command, HarnessCommand as H, Section, View};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Agents-tab and agent-detail key mapping. `f` opens an agent the way it
/// opens a session on the Sessions tab, and inside the detail it opens the
/// selected thing: a run's transcript, an artifact, a setting.
pub(super) fn map_harness_command(app: &App, key: &KeyEvent, on_agents: bool) -> Option<Command> {
    let editing = app
        .harness
        .detail
        .as_ref()
        .is_some_and(|d| d.editing.is_some());
    let cmd = match (&app.view, key.code) {
        (View::Grid, _) if !on_agents => return None,
        (View::Grid, KeyCode::Down | KeyCode::Char('j')) => H::NavDown,
        (View::Grid, KeyCode::Up | KeyCode::Char('k')) => H::NavUp,
        (View::Grid, KeyCode::Enter | KeyCode::Char('f') | KeyCode::Char('i')) => H::OpenDetail,
        (View::Grid, KeyCode::Char(' ')) => H::ToggleOn,
        (View::Grid, KeyCode::Char('p')) => H::RunNow,
        (View::Grid, KeyCode::Char('n')) => H::NewSession,

        // Typing a setting's value: every printable key is text.
        (View::AgentDetail, KeyCode::Esc) if editing => H::EditCancel,
        (View::AgentDetail, KeyCode::Enter) if editing => H::EditSubmit,
        (View::AgentDetail, KeyCode::Backspace) if editing => H::EditBackspace,
        (View::AgentDetail, KeyCode::Char(c))
            if editing && !key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            H::EditChar(c)
        }
        (View::AgentDetail, _) if editing => return None,

        (View::AgentDetail, KeyCode::Esc | KeyCode::Char('q')) => H::CloseDetail,
        (View::AgentDetail, KeyCode::Tab | KeyCode::Right | KeyCode::Char('l')) => H::NextSection,
        (View::AgentDetail, KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h')) => {
            H::PrevSection
        }
        (View::AgentDetail, KeyCode::Char(c @ '1'..='4')) => {
            H::ShowSection(Section::ALL[c as usize - '1' as usize])
        }
        (View::AgentDetail, KeyCode::Down | KeyCode::Char('j')) => H::DetailDown,
        (View::AgentDetail, KeyCode::Up | KeyCode::Char('k')) => H::DetailUp,
        (View::AgentDetail, KeyCode::Enter | KeyCode::Char('f') | KeyCode::Char('o')) => {
            H::Activate
        }
        (View::AgentDetail, KeyCode::Char('e')) => H::EditSetting,
        (View::AgentDetail, KeyCode::Char(' ')) => H::ToggleOn,
        (View::AgentDetail, KeyCode::Char('p')) => H::RunNow,
        (View::AgentDetail, KeyCode::Char('n')) => H::NewSession,
        (View::AgentDetail, KeyCode::Char('R')) => H::Reset,
        _ => return None,
    };
    Some(Command::Harness(cmd))
}
