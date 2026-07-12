use cc_hub_lib::app::{App, View};
use cc_hub_lib::{models, tmux_pane};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;

/// Handle Tasks board and Tasks modal input. Returns true when this feature
/// consumed the key, keeping the root dispatcher free of task workflow logic.
pub(super) fn handle(
    app: &mut App,
    key: KeyEvent,
    terminal: &Terminal<CrosstermBackend<io::Stdout>>,
    on_tasks: bool,
) -> bool {
    match (&app.view, key.code) {
        (View::Grid, KeyCode::Down | KeyCode::Char('j')) if on_tasks => app.tasks.row_down(),
        (View::Grid, KeyCode::Up | KeyCode::Char('k')) if on_tasks => app.tasks.row_up(),
        (View::Grid, KeyCode::Right | KeyCode::Char('l')) if on_tasks => app.tasks.col_right(),
        (View::Grid, KeyCode::Left | KeyCode::Char('h')) if on_tasks => app.tasks.col_left(),
        (View::Grid, KeyCode::Char('L')) if on_tasks => match app.move_selected_task(1) {
            Some(msg) => app.set_status(msg),
            None => app.set_status("nothing to move right".into()),
        },
        (View::Grid, KeyCode::Char('H')) if on_tasks => match app.move_selected_task(-1) {
            Some(msg) => app.set_status(msg),
            None => app.set_status("nothing to move left".into()),
        },
        (View::Grid, KeyCode::Char('a') | KeyCode::Char('n')) if on_tasks => app.enter_task_input(),
        (View::Grid, KeyCode::Char('/')) if on_tasks => app.enter_task_filter(),
        (View::Grid, KeyCode::Esc) if on_tasks && !app.tasks.filter.is_empty() => {
            app.clear_task_filter()
        }
        (View::Grid, KeyCode::Char('u')) if on_tasks => match app.undo_task_delete() {
            Some(msg) => app.set_status(msg),
            None => app.set_status("nothing to undo".into()),
        },
        (View::Grid, KeyCode::Char(' ')) if on_tasks => match app.task_space_action() {
            Some(msg) => app.set_status(msg),
            None => app.set_status("no task focused".into()),
        },
        (View::Grid, KeyCode::Char('s')) if on_tasks => {
            if !app.enter_task_assign_picker() {
                app.set_status("focus an unfinished task to assign an agent".into());
            }
        }
        (View::Grid, KeyCode::Char('S')) if on_tasks => match app.assign_selected_task_at_home() {
            Some(msg) => app.set_status(msg),
            None => app.set_status("focus a To-Do/In Progress task to start an agent".into()),
        },
        (View::Grid, KeyCode::Char('r')) if on_tasks => {
            if !app.enter_task_rename() {
                app.set_status("no task focused".into());
            }
        }
        (View::Grid, KeyCode::Char('t')) if on_tasks => {
            if !app.enter_task_tags() {
                app.set_status("no task focused".into());
            }
        }
        (View::Grid, KeyCode::Char(c @ ('1' | '2' | '3' | '4'))) if on_tasks => {
            use cc_hub_lib::tasks::TaskPriority;
            let priority = match c {
                '1' => TaskPriority::P1,
                '2' => TaskPriority::P2,
                '3' => TaskPriority::P3,
                _ => TaskPriority::P4,
            };
            match app.set_selected_task_priority(priority) {
                Some(msg) => app.set_status(msg),
                None => app.set_status("no task focused".into()),
            }
        }
        (View::Grid, KeyCode::Char('x')) if on_tasks => match app.delete_selected_task() {
            Some(msg) => app.set_status(msg),
            None => app.set_status("no task focused".into()),
        },
        (View::Grid, KeyCode::Char('c')) if on_tasks => app.clear_done_tasks(),
        (View::Grid, KeyCode::Char('f') | KeyCode::Enter) if on_tasks => {
            let Some(task) = app.selected_board_task().cloned() else {
                app.set_status("no task focused".into());
                return true;
            };
            let live_tmux = task
                .tmux
                .as_deref()
                .filter(|tmux| app.task_session_is_live(tmux));
            let tmux = if let Some(tmux) = live_tmux {
                tmux.to_string()
            } else if task.session_id.is_some() && task.cwd.is_some() {
                match app.resume_board_task(&task) {
                    Ok(tmux) => tmux,
                    Err(e) => {
                        app.set_status(e);
                        return true;
                    }
                }
            } else {
                app.set_status(if task.tmux.is_some() {
                    "agent session is gone and its session id was never seen — press s to re-assign"
                        .into()
                } else {
                    "no agent assigned — press s to assign one".into()
                });
                return true;
            };
            let (cols, rows) = crate::popup_pane_size(terminal);
            match tmux_pane::TmuxPaneView::spawn(&tmux, rows, cols) {
                Ok(pane) => {
                    if let Some(sid) = task.session_id.as_deref() {
                        app.set_status(format!("opened {} [{}]", models::short_sid(sid), tmux));
                    }
                    app.enter_tmux_pane(pane);
                }
                Err(e) => app.set_status(format!("tmux attach failed: {e}")),
            }
        }
        (View::TaskInput, KeyCode::Esc) => app.close_task_input(),
        (View::TaskInput, KeyCode::Backspace) => {
            app.tasks.input.pop();
        }
        (View::TaskInput, KeyCode::Enter) => {
            let renaming = app.tasks.renaming.is_some();
            if !app.submit_task_input() {
                let msg = app.tasks.take_persistence_error().unwrap_or_else(|| {
                    if renaming {
                        "empty task — rename cancelled".into()
                    } else {
                        "empty task — nothing added".into()
                    }
                });
                app.set_status(msg);
            }
        }
        (View::TaskInput, KeyCode::Char(c)) => app.tasks.input.push(c),
        (View::TaskTags, KeyCode::Esc) => app.close_task_tags(),
        (View::TaskTags, KeyCode::Backspace) => {
            app.tasks.input.pop();
        }
        (View::TaskTags, KeyCode::Enter) => {
            if !app.submit_task_tags() {
                if let Some(msg) = app.tasks.take_persistence_error() {
                    app.set_status(msg);
                }
            }
        }
        (View::TaskTags, KeyCode::Char(c)) => app.tasks.input.push(c),
        (View::TaskFilter, KeyCode::Esc) => app.clear_task_filter(),
        (View::TaskFilter, KeyCode::Enter) => app.apply_task_filter(),
        (View::TaskFilter, KeyCode::Backspace) => app.task_filter_pop(),
        (View::TaskFilter, KeyCode::Char(c)) => app.task_filter_push(c),
        _ => return false,
    }
    true
}
