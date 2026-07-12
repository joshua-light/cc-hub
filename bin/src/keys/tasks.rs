use cc_hub_lib::app::{App, Command, TasksCommand, View};
use crossterm::event::{KeyCode, KeyEvent};

/// Map a Tasks-board or Tasks-modal-submit key onto a [`Command`]. Guards and
/// arm order mirror the original inline match exactly: input-mode guards fire
/// before the Grid nav/action arms, and the Esc-clears-filter arm only claims
/// Esc while a filter is set (an unfiltered board's Esc falls through). Modal
/// character/backspace editing stays in [`handle`]; only the actions and the
/// input/tags/filter submit arms become commands.
pub(super) fn map_tasks_command(app: &App, key: &KeyEvent, on_tasks: bool) -> Option<Command> {
    use TasksCommand as T;
    let cmd = match (&app.view, key.code) {
        (View::Grid, KeyCode::Down | KeyCode::Char('j')) if on_tasks => T::NavDown,
        (View::Grid, KeyCode::Up | KeyCode::Char('k')) if on_tasks => T::NavUp,
        (View::Grid, KeyCode::Right | KeyCode::Char('l')) if on_tasks => T::NavRight,
        (View::Grid, KeyCode::Left | KeyCode::Char('h')) if on_tasks => T::NavLeft,
        (View::Grid, KeyCode::Char('L')) if on_tasks => T::MoveTaskRight,
        (View::Grid, KeyCode::Char('H')) if on_tasks => T::MoveTaskLeft,
        (View::Grid, KeyCode::Char('a') | KeyCode::Char('n')) if on_tasks => T::OpenAddInput,
        (View::Grid, KeyCode::Char('/')) if on_tasks => T::OpenFilter,
        (View::Grid, KeyCode::Esc) if on_tasks && !app.tasks.filter.is_empty() => T::ClearFilter,
        (View::Grid, KeyCode::Char('u')) if on_tasks => T::UndoDelete,
        (View::Grid, KeyCode::Char(' ')) if on_tasks => T::SpaceAction,
        (View::Grid, KeyCode::Char('s')) if on_tasks => T::OpenAssignPicker,
        (View::Grid, KeyCode::Char('S')) if on_tasks => T::AssignAtHome,
        (View::Grid, KeyCode::Char('r')) if on_tasks => T::OpenRename,
        (View::Grid, KeyCode::Char('t')) if on_tasks => T::OpenTags,
        (View::Grid, KeyCode::Char(c @ ('1' | '2' | '3' | '4'))) if on_tasks => {
            use cc_hub_lib::orchestrator::TaskPriority;
            let priority = match c {
                '1' => TaskPriority::P1,
                '2' => TaskPriority::P2,
                '3' => TaskPriority::P3,
                _ => TaskPriority::P4,
            };
            T::SetPriority(priority)
        }
        (View::Grid, KeyCode::Char('x')) if on_tasks => T::DeleteSelected,
        (View::Grid, KeyCode::Char('c')) if on_tasks => T::ClearDone,
        (View::Grid, KeyCode::Char('P')) if on_tasks => T::PromoteSelected,
        (View::Grid, KeyCode::Char('f') | KeyCode::Enter) if on_tasks => T::FocusAgent,
        (View::TaskInput, KeyCode::Enter) => T::SubmitInput,
        (View::TaskTags, KeyCode::Enter) => T::SubmitTags,
        (View::TaskFilter, KeyCode::Enter) => T::ApplyFilter,
        _ => return None,
    };
    Some(Command::Tasks(cmd))
}

/// Handle the Tasks modal buffer-edit keys that stay in `bin` (typing into the
/// add/rename, tag, and filter buffers, plus their Esc/Backspace). Board
/// actions and modal submits are commands (see [`map_tasks_command`]); this
/// runs only after `map_tasks_command` returns `None`. Returns true when it
/// consumed the key.
pub(super) fn handle(app: &mut App, key: KeyEvent) -> bool {
    match (&app.view, key.code) {
        (View::TaskInput, KeyCode::Esc) => app.close_task_input(),
        (View::TaskInput, KeyCode::Backspace) => {
            app.tasks.input.pop();
        }
        (View::TaskInput, KeyCode::Char(c)) => app.tasks.input.push(c),
        (View::TaskTags, KeyCode::Esc) => app.close_task_tags(),
        (View::TaskTags, KeyCode::Backspace) => {
            app.tasks.input.pop();
        }
        (View::TaskTags, KeyCode::Char(c)) => app.tasks.input.push(c),
        (View::TaskFilter, KeyCode::Esc) => app.clear_task_filter(),
        (View::TaskFilter, KeyCode::Backspace) => app.task_filter_pop(),
        (View::TaskFilter, KeyCode::Char(c)) => app.task_filter_push(c),
        _ => return false,
    }
    true
}
