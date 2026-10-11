use cc_hub_lib::app::{App, BuildsCommand as B, Command, View};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Builds-tab, new-build form and build-log key mapping. The list and the
/// log share the recipe verbs, so a build can be run or cancelled while its
/// log has the keyboard. In the form every printable key types into a text
/// field, so the form's own keys are the unprintable ones.
pub(super) fn map_builds_command(app: &App, key: &KeyEvent, on_builds: bool) -> Option<Command> {
    let cmd = match (&app.view, key.code) {
        (View::Grid, _) if !on_builds => return None,
        (View::Grid, KeyCode::Up | KeyCode::Char('k')) => B::NavUp,
        (View::Grid, KeyCode::Down | KeyCode::Char('j')) => B::NavDown,
        (View::Grid, KeyCode::Enter | KeyCode::Char('f')) => B::OpenLog,
        (View::Grid | View::BuildLog, KeyCode::Char('n')) => B::OpenForm,
        (View::Grid | View::BuildLog, KeyCode::Char('r')) => B::Run,
        (View::Grid | View::BuildLog, KeyCode::Char('c')) => B::Cancel,
        (View::Grid | View::BuildLog, KeyCode::Char(' ')) => B::ToggleHold,

        (View::BuildForm, KeyCode::Esc) => B::FormCancel,
        (View::BuildForm, KeyCode::Enter) => B::FormSubmit,
        (View::BuildForm, KeyCode::Tab | KeyCode::Down) => B::FormNext,
        (View::BuildForm, KeyCode::BackTab | KeyCode::Up) => B::FormPrev,
        (View::BuildForm, KeyCode::Left) => B::FormLeft,
        (View::BuildForm, KeyCode::Right) => B::FormRight,
        (View::BuildForm, KeyCode::Backspace) => B::FormBackspace,
        (View::BuildForm, KeyCode::Char(c)) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            B::FormChar(c)
        }
        (View::BuildForm, _) => return None,

        (View::BuildLog, KeyCode::Esc | KeyCode::F(1) | KeyCode::Char('q')) => B::CloseLog,
        (View::BuildLog, KeyCode::Up | KeyCode::Char('k')) => B::LogUp,
        (View::BuildLog, KeyCode::Down | KeyCode::Char('j')) => B::LogDown,
        (View::BuildLog, KeyCode::PageUp) => B::LogPageUp,
        (View::BuildLog, KeyCode::PageDown) => B::LogPageDown,
        (View::BuildLog, KeyCode::Char('G') | KeyCode::End) => B::LogEnd,
        _ => return None,
    };
    Some(Command::Builds(cmd))
}
