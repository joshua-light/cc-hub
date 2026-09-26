//! The task-link picker (`L` on the Sessions tab,
//! [`crate::app::View::TaskLinkPicker`]): link the selected session to a
//! board task, or unlink it. Candidates are banded by board column, tasks
//! local to the session's cwd first.

use crate::app::picker_list::{rank_rows, step, PickerRow, Searchable};
use crate::app::{App, View};
use crate::models::SessionInfo;
use crate::tasks::store::{TaskState, TaskStatus};

/// What picking a row does: drop the session's current link, or point it at
/// a task. `Link` carries everything the sidecar record needs so the confirm
/// path never has to re-resolve the task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskLinkAction {
    Unlink,
    Link { task_id: String, title: String },
}

/// One candidate row: `label` is the task title (or prompt first line),
/// `detail` the status's board label (so typing "in progress" narrows to
/// that band). `status` drives the renderer's status coloring — `None` on
/// the unlink row.
#[derive(Clone, Debug)]
pub struct TaskLinkChoice {
    pub label: String,
    pub detail: String,
    pub status: Option<TaskStatus>,
    pub action: TaskLinkAction,
}

#[derive(Clone, Debug)]
pub struct TaskLinkPickerState {
    /// Session being linked, captured at open time so a rescan can't move
    /// the target under the popup.
    pub session_id: String,
    /// Display name for the popup footer (session title or short id).
    pub session_label: String,
    pub selected: usize,
    pub filter: String,
    pub rows: Vec<PickerRow>,
    pub choices: Vec<TaskLinkChoice>,
}

impl TaskLinkPickerState {
    /// `current_task_id` pre-selects the row of the task the session is
    /// already linked to, so `L` opens focused on the status quo.
    pub(crate) fn new(
        session_id: String,
        session_label: String,
        choices: Vec<TaskLinkChoice>,
        current_task_id: Option<&str>,
    ) -> Self {
        let mut picker = Self {
            session_id,
            session_label,
            selected: 0,
            filter: String::new(),
            rows: Vec::new(),
            choices,
        };
        picker.refilter();
        if let Some(current) = current_task_id {
            if let Some(idx) = picker.rows.iter().position(|row| {
                matches!(
                    &picker.choices[row.choice].action,
                    TaskLinkAction::Link { task_id, .. } if task_id == current
                )
            }) {
                picker.selected = idx;
            }
        }
        picker
    }

    pub fn push_filter(&mut self, c: char) {
        self.filter.push(c);
        self.refilter();
    }

    pub fn pop_filter(&mut self) {
        self.filter.pop();
        self.refilter();
    }

    pub fn move_selection(&mut self, delta: isize) {
        self.selected = step(self.selected, delta, self.rows.len());
    }

    pub fn selected_action(&self) -> Option<&TaskLinkAction> {
        self.rows
            .get(self.selected)
            .and_then(|row| self.choices.get(row.choice))
            .map(|choice| &choice.action)
    }

    fn refilter(&mut self) {
        self.rows = rank_rows(
            &self.filter,
            self.choices.iter().map(|task| Searchable {
                label: &task.label,
                detail: &task.detail,
                id: None,
            }),
        );
        self.selected = 0;
    }
}

/// A task's display label: its Haiku title when present, else the first
/// line of the prompt truncated to fit picker rows and group headers.
pub(super) fn task_display_title(task: &TaskState) -> String {
    task.title
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| crate::models::first_line_truncated(&task.prompt, 48))
}

/// Task-link picker band order: the Tasks-board columns left to right
/// (To-Do → Planning → In Progress → Review → Done).
fn task_link_status_rank(status: TaskStatus) -> u8 {
    match status {
        TaskStatus::Backlog => 0,
        TaskStatus::Planning => 1,
        TaskStatus::Running => 2,
        TaskStatus::Review => 3,
        TaskStatus::Done => 4,
    }
}

/// One task-link picker candidate plus its sort key parts:
/// `(status band, not-local-to-the-session's-cwd, updated_at)`. The detail
/// is just the status board label.
fn task_link_candidate(task: &TaskState, session_cwd: &str) -> (u8, bool, i64, TaskLinkChoice) {
    let title = task_display_title(task);
    // "Local" means the task's board assignment ran in the session's cwd —
    // those tasks lead their band.
    let local = task.cwd.as_deref() == Some(session_cwd);
    let choice = TaskLinkChoice {
        label: title.clone(),
        detail: task.status.board_label().to_string(),
        status: Some(task.status),
        action: TaskLinkAction::Link {
            task_id: task.task_id.clone(),
            title,
        },
    };
    (
        task_link_status_rank(task.status),
        !local,
        task.updated_at,
        choice,
    )
}

impl App {
    /// `L` on the Sessions tab: open the fuzzy task selector to link the
    /// selected session to a task (or unlink it). The target session is
    /// captured now so a rescan can't move it under the popup. Returns
    /// `false` when nothing is selected or there is nothing to offer.
    pub fn enter_task_link_picker(&mut self) -> bool {
        let Some(session) = self.selected_session_info().cloned() else {
            return false;
        };
        let current = self.session_task_links.get(&session.session_id).cloned();
        let mut choices: Vec<TaskLinkChoice> = Vec::new();
        if current.is_some() {
            choices.push(TaskLinkChoice {
                label: "✕ unlink".into(),
                detail: "remove the task link".into(),
                status: None,
                action: TaskLinkAction::Unlink,
            });
        }
        choices.extend(self.task_link_candidates(&session));
        if choices.is_empty() {
            return false;
        }
        let label = session
            .title
            .clone()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| crate::models::short_sid(&session.session_id).to_string());
        self.task_link_picker = Some(TaskLinkPickerState::new(
            session.session_id.clone(),
            label,
            choices,
            current.as_ref().map(|l| l.task_id.as_str()),
        ));
        self.view = View::TaskLinkPicker;
        true
    }

    /// Every board task as picker rows, banded by status in Tasks-board
    /// column order (To-Do → Planning → In Progress → Review → Done); within
    /// a band, tasks local to the session's cwd first, then newest first.
    fn task_link_candidates(&self, session: &SessionInfo) -> Vec<TaskLinkChoice> {
        let mut candidates: Vec<(u8, bool, i64, TaskLinkChoice)> = self
            .tasks
            .board
            .tasks()
            .iter()
            .map(|t| task_link_candidate(t, &session.cwd))
            .collect();
        candidates.sort_by_key(|(band, not_local, updated_at, choice)| {
            (
                *band,
                *not_local,
                std::cmp::Reverse(*updated_at),
                choice.label.to_lowercase(),
            )
        });
        candidates.into_iter().map(|(_, _, _, c)| c).collect()
    }

    pub fn close_task_link_picker(&mut self) {
        self.task_link_picker = None;
        self.view = View::Grid;
    }

    /// Move the task-link-picker highlight by `delta` rows, clamped to the
    /// live filtered result list.
    pub fn task_link_picker_move(&mut self, delta: isize) {
        if let Some(picker) = self.task_link_picker.as_mut() {
            picker.move_selection(delta);
        }
    }

    /// Enter on the task-link picker: persist the link (or unlink) for the
    /// captured session and regroup the grid immediately. An empty match
    /// list keeps the picker open, mirroring the model picker.
    pub fn confirm_task_link_picker(&mut self) {
        let Some(action) = self
            .task_link_picker
            .as_ref()
            .and_then(TaskLinkPickerState::selected_action)
            .cloned()
        else {
            return;
        };
        let Some(picker) = self.task_link_picker.take() else {
            return;
        };
        self.view = View::Grid;
        let sid = picker.session_id;
        let mut linked = false;
        let status = match action {
            TaskLinkAction::Unlink => match crate::tasks::session_links::unlink(&sid) {
                Ok(()) => {
                    self.session_task_links.remove(&sid);
                    linked = true;
                    "task link removed".to_string()
                }
                Err(e) => {
                    log::warn!("task link: unlink failed for {}: {}", sid, e);
                    format!("unlink failed: {}", e)
                }
            },
            TaskLinkAction::Link { task_id, title } => {
                let link = crate::tasks::session_links::TaskLink {
                    task_id,
                    title: title.clone(),
                };
                match crate::tasks::session_links::link(&sid, link.clone()) {
                    Ok(()) => {
                        self.session_task_links.insert(sid.clone(), link);
                        linked = true;
                        format!("linked to “{}”", title)
                    }
                    Err(e) => {
                        log::warn!("task link: persist failed for {}: {}", sid, e);
                        format!("link failed: {}", e)
                    }
                }
            }
        };
        // Links shape card order (`cluster_by_task`), so regroup now rather
        // than on the next scan tick about a second later. `adopt_groups`
        // keeps the cursor on the same session id, so it rides with the card.
        if linked {
            self.rebuild_groups();
        }
        self.set_status(status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link_choice(label: &str, task_id: &str) -> TaskLinkChoice {
        TaskLinkChoice {
            label: label.into(),
            detail: "In Progress".into(),
            status: Some(TaskStatus::Running),
            action: TaskLinkAction::Link {
                task_id: task_id.into(),
                title: label.into(),
            },
        }
    }

    #[test]
    fn preselects_current_link_after_the_unlink_row() {
        let choices = vec![
            TaskLinkChoice {
                label: "✕ unlink".into(),
                detail: String::new(),
                status: None,
                action: TaskLinkAction::Unlink,
            },
            link_choice("Fix auth", "tk-1"),
            link_choice("Ship parser", "tk-2"),
        ];
        let picker = TaskLinkPickerState::new("sid".into(), "sid".into(), choices, Some("tk-2"));
        assert_eq!(picker.selected, 2);
        assert!(matches!(
            picker.selected_action(),
            Some(TaskLinkAction::Link { task_id, .. }) if task_id == "tk-2"
        ));
    }

    #[test]
    fn filter_narrows_and_highlights_labels() {
        let choices = vec![
            link_choice("Fix auth", "tk-1"),
            link_choice("Ship it", "tk-2"),
        ];
        let mut picker = TaskLinkPickerState::new("sid".into(), "sid".into(), choices, None);
        assert_eq!(picker.rows.len(), 2);
        for c in "fixa".chars() {
            picker.push_filter(c);
        }
        assert_eq!(picker.rows.len(), 1);
        assert!(!picker.rows[0].label_indices.is_empty());
        assert!(matches!(
            picker.selected_action(),
            Some(TaskLinkAction::Link { task_id, .. }) if task_id == "tk-1"
        ));
        picker.pop_filter();
        picker.pop_filter();
        picker.pop_filter();
        picker.pop_filter();
        assert_eq!(picker.rows.len(), 2);
    }

    #[test]
    fn no_match_yields_no_action() {
        let mut picker = TaskLinkPickerState::new(
            "sid".into(),
            "sid".into(),
            vec![link_choice("Fix auth", "tk-1")],
            None,
        );
        for c in "zzz".chars() {
            picker.push_filter(c);
        }
        assert!(picker.rows.is_empty());
        assert!(picker.selected_action().is_none());
    }
}

#[cfg(all(test, unix))]
mod app_tests {
    use super::*;
    use crate::app::test_support::{app_with, fake_session, session, status};
    use crate::app::{Command, SessionsCommand};
    use crate::models::SessionState;

    // Linking reorders the grid on the keypress itself, not on the next
    // scan tick about a second later.
    #[test]
    fn confirm_task_link_regroups_without_a_scan() {
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            app.tasks.board.add("ship it").unwrap().unwrap();
            let a = fake_session("s-a", SessionState::Processing);
            let b = fake_session("s-b", SessionState::Processing);
            assert!(app.update_sessions(vec![a, b]));

            // Link the trailing card; its cluster has to jump the unlinked one.
            app.sessions.sel_group = 0;
            app.sessions.sel_in_group = 1;
            assert_eq!(app.selected_session_id().as_deref(), Some("s-b"));
            assert!(app.enter_task_link_picker());
            app.confirm_task_link_picker();

            let order: Vec<&str> = app.sessions.groups[0]
                .sessions
                .iter()
                .map(|s| s.session_id.as_str())
                .collect();
            assert_eq!(order, vec!["s-b", "s-a"]);
            // The cursor rides along with the card it moved.
            assert_eq!(app.selected_session_id().as_deref(), Some("s-b"));

            // Unlinking regroups on the same keypress too. The picker opens
            // with the linked task highlighted, so walk up to "✕ unlink".
            assert!(app.enter_task_link_picker());
            app.task_link_picker_move(-10);
            app.confirm_task_link_picker();
            let order: Vec<&str> = app.sessions.groups[0]
                .sessions
                .iter()
                .map(|s| s.session_id.as_str())
                .collect();
            assert_eq!(order, vec!["s-a", "s-b"]);
            assert_eq!(app.selected_session_id().as_deref(), Some("s-b"));
        });
    }

    #[test]
    fn task_link_candidates_band_in_tasks_board_column_order() {
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            // Insert out of band order so the sort has to do the work.
            let done = app.tasks.board.add("done task").unwrap().unwrap();
            app.tasks.board.set_status(&done, TaskStatus::Done).unwrap();
            let running = app.tasks.board.add("running task").unwrap().unwrap();
            app.tasks
                .board
                .set_status(&running, TaskStatus::Running)
                .unwrap();
            app.tasks.board.add("todo task").unwrap().unwrap();
            let planning = app.tasks.board.add("planning task").unwrap().unwrap();
            app.tasks
                .board
                .set_status(&planning, TaskStatus::Planning)
                .unwrap();

            let session = fake_session("s-1", SessionState::Idle);
            let choices = app.task_link_candidates(&session);
            let labels: Vec<&str> = choices.iter().map(|c| c.label.as_str()).collect();
            assert_eq!(
                labels,
                vec!["todo task", "planning task", "running task", "done task"]
            );
            // Details carry the board label (not the wire name), and the
            // status rides along for the renderer's coloring.
            assert_eq!(choices[0].detail, "To-Do");
            assert_eq!(choices[0].status, Some(TaskStatus::Backlog));
            assert_eq!(choices[2].detail, "In Progress");
        });
    }

    #[test]
    fn task_link_picker_links_then_unlinks_selected_session() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = app_with(vec![session("sid-1", SessionState::Idle, None)]);
            let tid = app.tasks.board.add("fix the auth flow").unwrap().unwrap();

            // First open: no link yet, so no unlink row; Enter links.
            let effects = app.execute(Command::Sessions(SessionsCommand::OpenTaskLinkPicker));
            assert!(effects.is_empty());
            assert_eq!(app.view, crate::app::View::TaskLinkPicker);
            {
                let picker = app.task_link_picker.as_ref().expect("picker state");
                assert!(picker
                    .choices
                    .iter()
                    .all(|c| c.action != crate::app::TaskLinkAction::Unlink));
            }
            app.confirm_task_link_picker();
            assert_eq!(app.view, crate::app::View::Grid);
            assert_eq!(
                crate::tasks::session_links::load()
                    .get("sid-1")
                    .unwrap()
                    .task_id,
                tid
            );
            // The card badge resolves live (non-stale) from the board task;
            // the grid's groups are untouched by links.
            let badge = app.task_badge("sid-1").expect("badge");
            assert_eq!(badge.task_id, tid);
            assert!(!badge.stale);
            assert!(
                status(&app).starts_with("linked to"),
                "got: {}",
                status(&app)
            );

            // Second open: the unlink row leads and the linked task is
            // pre-selected; picking unlink drops the link and the badge.
            app.execute(Command::Sessions(SessionsCommand::OpenTaskLinkPicker));
            {
                let picker = app.task_link_picker.as_ref().expect("picker state");
                assert_eq!(picker.choices[0].action, crate::app::TaskLinkAction::Unlink);
                assert!(matches!(
                    picker.selected_action(),
                    Some(crate::app::TaskLinkAction::Link { task_id, .. }) if *task_id == tid
                ));
            }
            app.task_link_picker.as_mut().unwrap().move_selection(-100);
            app.confirm_task_link_picker();
            assert!(crate::tasks::session_links::load().is_empty());
            assert!(app.task_badge("sid-1").is_none());
            assert_eq!(status(&app), "task link removed");
        });
    }

    #[test]
    fn task_link_picker_without_tasks_reports_instead_of_opening() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = app_with(vec![session("sid-1", SessionState::Idle, None)]);
            app.execute(Command::Sessions(SessionsCommand::OpenTaskLinkPicker));
            assert_eq!(app.view, crate::app::View::Grid);
            assert!(app.task_link_picker.is_none());
            assert!(
                status(&app).starts_with("no tasks to link"),
                "got: {}",
                status(&app)
            );
        });
    }
}
