use super::Effect;
use crate::app::App;
use crate::config;
use crate::models;
use crate::tasks::store::TaskPriority;

/// Tasks-tab commands: the grid actions and the input/tags/filter submit
/// arms. Typing into those buffers stays in `bin/src/keys/tasks.rs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TasksCommand {
    NavUp,
    NavDown,
    NavLeft,
    NavRight,
    /// `L` — move the focused card one column right.
    MoveTaskRight,
    /// `H` — move the focused card one column left.
    MoveTaskLeft,
    /// `a`/`n` — open the add-task input.
    OpenAddInput,
    /// `/` — open the filter bar.
    OpenFilter,
    /// Esc on a filtered board — drop the filter.
    ClearFilter,
    /// `u` — restore the last delete/clear-done batch.
    UndoDelete,
    /// Space — proceed a Planning card or toggle Done.
    SpaceAction,
    /// `s` — open the agent-assign places picker.
    OpenAssignPicker,
    /// `S` — assign the focused task an agent at `$HOME`.
    AssignAtHome,
    /// `r` — open the rename popup for the focused card.
    OpenRename,
    /// `T` — open the deliverable-kind picker for the focused card.
    OpenKindPicker,
    /// `t` — open the tag editor for the focused card.
    OpenTags,
    /// `1`–`4` — set the focused card's priority.
    SetPriority(TaskPriority),
    /// `x` — delete the focused card (undoable).
    DeleteSelected,
    /// `c` — clear the Done column (undoable).
    ClearDone,
    /// `f`/Enter — attach a live agent, resume a dead one, or explain.
    FocusAgent,
    /// `v` — open the Task Info popup (prompt + attachments) for the
    /// focused card.
    OpenTaskInfo,
    /// `A` on the grid / `a` inside the Task Info popup — open the attach
    /// input (paste a file path or URL). `from_info` routes submit/cancel
    /// back to where it was opened.
    OpenAttachInput {
        from_info: bool,
    },
    /// Attach-input Enter.
    SubmitAttach,
    /// `x` inside the Task Info popup — remove the selected attachment.
    RemoveAttachment,
    /// Task-input Enter (add or rename).
    SubmitInput,
    /// Task-tags Enter.
    SubmitTags,
    /// Task-filter Enter (keep the query applied).
    ApplyFilter,
}

impl App {
    pub(super) fn execute_tasks(&mut self, cmd: TasksCommand) -> Vec<Effect> {
        use TasksCommand::*;
        match cmd {
            NavRight => self.tasks.col_right(),
            NavLeft => self.tasks.col_left(),
            NavDown => self.tasks.row_down(),
            NavUp => self.tasks.row_up(),
            MoveTaskRight => {
                let msg = self.move_selected_task(1);
                self.status_or(msg, "nothing to move right");
            }
            MoveTaskLeft => {
                let msg = self.move_selected_task(-1);
                self.status_or(msg, "nothing to move left");
            }
            OpenAddInput => self.enter_task_input(),
            OpenFilter => self.enter_task_filter(),
            ClearFilter => self.clear_task_filter(),
            UndoDelete => {
                let msg = self.undo_task_delete();
                self.status_or(msg, "nothing to undo");
            }
            SpaceAction => {
                let msg = self.task_space_action();
                self.status_or(msg, "no task focused");
            }
            OpenAssignPicker => {
                if !self.enter_task_assign_picker() {
                    self.set_status("focus an unfinished task to assign an agent".into());
                }
            }
            AssignAtHome => {
                let msg = self.assign_selected_task_at_home();
                self.status_or(msg, "focus a To-Do/In Progress task to start an agent");
            }
            OpenRename => {
                if !self.enter_task_rename() {
                    self.set_status("no task focused".into());
                }
            }
            OpenTags => {
                if !self.enter_task_tags() {
                    self.set_status("no task focused".into());
                }
            }
            OpenKindPicker => {
                if !self.enter_task_kind_picker() {
                    let why = if config::get().tasks.kinds.is_empty() {
                        "no task kinds configured — set [tasks].kinds in ~/.cc-hub/config.toml"
                    } else {
                        "no task focused"
                    };
                    self.set_status(why.into());
                }
            }
            SetPriority(priority) => {
                let msg = self.set_selected_task_priority(priority);
                self.status_or(msg, "no task focused");
            }
            DeleteSelected => {
                let msg = self.delete_selected_task();
                self.status_or(msg, "no task focused");
            }
            ClearDone => self.clear_done_tasks(),
            FocusAgent => return self.focus_task_agent(),
            OpenTaskInfo => {
                if !self.enter_task_info() {
                    self.set_status("no task focused".into());
                }
            }
            OpenAttachInput { from_info } => {
                if !self.enter_task_attach(from_info) {
                    self.set_status("no task focused".into());
                }
            }
            SubmitAttach => {
                if !self.submit_task_attach() {
                    let msg = self
                        .tasks
                        .take_persistence_error()
                        .unwrap_or_else(|| "attach cancelled — empty input".into());
                    self.set_status(msg);
                }
            }
            RemoveAttachment => {
                let msg = self.remove_selected_attachment();
                self.status_or(msg, "no task focused");
            }
            SubmitInput => {
                let renaming = self.tasks.renaming.is_some();
                if !self.submit_task_input() {
                    let msg = self.tasks.take_persistence_error().unwrap_or_else(|| {
                        if renaming {
                            "empty task — rename cancelled".into()
                        } else {
                            "empty task — nothing added".into()
                        }
                    });
                    self.set_status(msg);
                }
            }
            SubmitTags => {
                if !self.submit_task_tags() {
                    if let Some(msg) = self.tasks.take_persistence_error() {
                        self.set_status(msg);
                    }
                }
            }
            ApplyFilter => self.apply_task_filter(),
        }
        Vec::new()
    }

    /// Report the action's own message, or `fallback` when it had nothing
    /// to act on.
    fn status_or(&mut self, msg: Option<String>, fallback: &str) {
        self.set_status(msg.unwrap_or_else(|| fallback.into()));
    }

    /// `f`/Enter on a board card: attach a live agent's tmux pane, resume a
    /// dead-but-resumable session in place (runtime spawn, rebinds `tmux`), or
    /// explain why neither is possible. Both attach paths end in a pane
    /// effect; pane sizing is bin's job.
    fn focus_task_agent(&mut self) -> Vec<Effect> {
        let Some(task) = self.selected_board_task().cloned() else {
            self.set_status("no task focused".into());
            return Vec::new();
        };
        let live_tmux = task
            .tmux
            .as_deref()
            .filter(|tmux| self.task_session_is_live(tmux));
        let tmux = match live_tmux {
            Some(tmux) => tmux.to_string(),
            None if task.session_id.is_some() && task.cwd.is_some() => {
                match self.resume_board_task(&task) {
                    Ok(tmux) => tmux,
                    Err(e) => {
                        self.set_status(e);
                        return Vec::new();
                    }
                }
            }
            None => {
                self.set_status(if task.tmux.is_some() {
                    "agent session is gone and its session id was never seen — press s to re-assign"
                        .into()
                } else {
                    "no agent assigned — press s to assign one".into()
                });
                return Vec::new();
            }
        };
        if let Some(sid) = task.session_id.as_deref() {
            self.set_status(format!("opened {} [{}]", models::short_sid(sid), tmux));
        }
        vec![Effect::OpenTmuxPane { tmux, owned: false }]
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::test_support::{session, status, task_app, tasks};
    use crate::app::PROCEED_PROMPT;
    use crate::models::SessionState;
    use crate::tasks::store::TaskStatus;

    #[test]
    fn space_action_walks_planning_running_done_and_reopens() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = task_app();
            let id = app.tasks.board.add("ship it").unwrap().unwrap();
            // assign lands the card in Planning with a live tmux binding.
            app.tasks
                .board
                .assign(&id, "/tmp/proj", "claude", "mux-1")
                .unwrap();
            app.focus_task(&id);

            // Space on a Planning card proceeds: send_prompt to the live tmux,
            // card → Running.
            tasks(&mut app, TasksCommand::SpaceAction);
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Running
            );
            assert_eq!(
                runtime.prompts.lock().unwrap().as_slice(),
                &[("mux-1".into(), PROCEED_PROMPT.into())]
            );

            // Space again finishes: card → Done, live session killed.
            app.focus_task(&id);
            tasks(&mut app, TasksCommand::SpaceAction);
            assert_eq!(app.tasks.board.get(&id).unwrap().status, TaskStatus::Done);
            assert_eq!(runtime.kills.lock().unwrap().as_slice(), &["mux-1"]);

            // Space on the Done card reopens it into Backlog (To-Do).
            app.focus_task(&id);
            tasks(&mut app, TasksCommand::SpaceAction);
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Backlog
            );
        });
    }

    #[test]
    fn focus_agent_live_tmux_opens_pane() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = task_app();
            let id = app.tasks.board.add("look at it").unwrap().unwrap();
            app.tasks
                .board
                .assign(&id, "/tmp/proj", "claude", "mux-live")
                .unwrap();
            app.focus_task(&id);
            runtime
                .exists
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let effects = tasks(&mut app, TasksCommand::FocusAgent);
            assert_eq!(
                effects,
                vec![Effect::OpenTmuxPane {
                    tmux: "mux-live".into(),
                    owned: false
                }]
            );
            // Live attach never respawns.
            assert!(runtime.spawns.lock().unwrap().is_empty());
        });
    }

    #[test]
    fn focus_agent_dead_tmux_resumes_and_rebinds() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = task_app();
            let id = app.tasks.board.add("resume me").unwrap().unwrap();
            app.tasks
                .board
                .assign(&id, "/tmp/proj", "claude", "mux-dead")
                .unwrap();
            // Give the card a resumable session id (assign clears it) by
            // binding a scanned session that matches its tmux name.
            app.tasks
                .board
                .bind_sessions(&[session("sid-resume", SessionState::Idle, Some("mux-dead"))])
                .unwrap();
            app.focus_task(&id);
            // tmux is dead now; resume spawns a fresh session.
            runtime
                .exists
                .store(false, std::sync::atomic::Ordering::Relaxed);
            let effects = tasks(&mut app, TasksCommand::FocusAgent);
            assert_eq!(
                effects,
                vec![Effect::OpenTmuxPane {
                    tmux: "mock-spawn".into(),
                    owned: false
                }]
            );
            let spawns = runtime.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1);
            assert_eq!(spawns[0].resume.as_deref(), Some("Resume(\"sid-resume\")"));
            // The binding now points at the freshly-spawned session.
            assert_eq!(
                app.tasks.board.get(&id).unwrap().tmux.as_deref(),
                Some("mock-spawn")
            );
        });
    }

    #[test]
    fn focus_agent_without_binding_gives_guidance() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = task_app();
            let id = app.tasks.board.add("unassigned").unwrap().unwrap();
            app.focus_task(&id);
            let effects = tasks(&mut app, TasksCommand::FocusAgent);
            assert!(effects.is_empty());
            assert!(runtime.spawns.lock().unwrap().is_empty());
            assert_eq!(status(&app), "no agent assigned — press s to assign one");
        });
    }

    #[test]
    fn delete_then_undo_restores_card() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("delete me").unwrap().unwrap();
            app.focus_task(&id);
            tasks(&mut app, TasksCommand::DeleteSelected);
            assert!(app.tasks.board.get(&id).is_none());
            tasks(&mut app, TasksCommand::UndoDelete);
            assert!(app.tasks.board.get(&id).is_some());
        });
    }

    #[test]
    fn set_priority_updates_card() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("prioritize me").unwrap().unwrap();
            app.focus_task(&id);
            tasks(&mut app, TasksCommand::SetPriority(TaskPriority::P1));
            assert_eq!(app.tasks.board.get(&id).unwrap().priority, TaskPriority::P1);
        });
    }

    #[test]
    fn move_task_right_and_left_walk_columns() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("walk me").unwrap().unwrap();
            app.focus_task(&id);
            // Manual moves hop over Planning: Backlog → Running → Review →
            // Done.
            tasks(&mut app, TasksCommand::MoveTaskRight);
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Running
            );
            tasks(&mut app, TasksCommand::MoveTaskRight);
            assert_eq!(app.tasks.board.get(&id).unwrap().status, TaskStatus::Review);
            tasks(&mut app, TasksCommand::MoveTaskRight);
            assert_eq!(app.tasks.board.get(&id).unwrap().status, TaskStatus::Done);
            // And back: Done → Review → Running → Backlog.
            tasks(&mut app, TasksCommand::MoveTaskLeft);
            assert_eq!(app.tasks.board.get(&id).unwrap().status, TaskStatus::Review);
            tasks(&mut app, TasksCommand::MoveTaskLeft);
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Running
            );
            tasks(&mut app, TasksCommand::MoveTaskLeft);
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Backlog
            );
        });
    }
}
