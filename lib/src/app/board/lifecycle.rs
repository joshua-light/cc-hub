//! Status transitions on a card: approve a plan, finish, move by hand,
//! reprioritise, delete and undo.

use crate::app::App;
use crate::tasks::store::{TaskPriority, TaskState, TaskStatus};

/// What Space on a Planning card sends to the bound agent. Kept terse: the
/// plan-first framing in [`planning_prompt`](super::assign::planning_prompt) already told the agent what
/// "proceed" means.
pub const PROCEED_PROMPT: &str = "Proceed with the implementation.";

/// A card's prompt as quoted in status lines.
fn card_preview(t: &TaskState) -> String {
    crate::models::first_line_truncated(&t.prompt, 32)
}

impl App {
    /// Space on the board, status-aware: a Planning card tells its agent to
    /// proceed with the implementation; anything else toggles Done. Returns
    /// `None` when no task is focused.
    pub fn task_space_action(&mut self) -> Option<String> {
        match self.selected_board_task()?.status {
            TaskStatus::Planning => Some(self.proceed_selected_task()),
            _ => self.toggle_task_done(),
        }
    }

    /// Approve the focused Planning card's plan: deliver
    /// [`PROCEED_PROMPT`] to the bound agent and move the card to In
    /// Progress. A live session gets the prompt immediately (if the agent is
    /// still planning, it lands as the queued next message); a dead tmux
    /// with a known session id is respawned with resume and the prompt
    /// queued for dispatch once it reports Idle. With nothing to deliver to,
    /// the card stays in Planning so the column never lies about an agent
    /// actually implementing.
    pub fn proceed_selected_task(&mut self) -> String {
        let Some(t) = self.selected_board_task() else {
            return "no task focused".into();
        };
        let id = t.task_id.clone();
        let preview = card_preview(t);
        let live_tmux = t.tmux.clone().filter(|n| self.runtime.session_exists(n));
        if let Some(tmux) = live_tmux {
            return match self.runtime.send_prompt(&tmux, PROCEED_PROMPT) {
                Ok(()) => {
                    if let Err(e) = self.tasks.board.set_status(&id, TaskStatus::Running) {
                        return format!("proceed sent but task state write failed: {e}");
                    }
                    self.focus_task(&id);
                    format!(
                        "proceeding: {} — agent told to implement [{}]",
                        preview, tmux
                    )
                }
                Err(e) => format!("proceed failed: {} — task stays in planning", e),
            };
        }
        let (sid, cwd, agent_id) = {
            let Some(t) = self.tasks.board.get(&id) else {
                return "task vanished".into();
            };
            match (t.session_id.clone(), t.cwd.clone()) {
                (Some(sid), Some(cwd)) => (
                    sid,
                    cwd,
                    t.agent_id.clone().unwrap_or_else(|| "claude".into()),
                ),
                _ => {
                    return "agent session is gone and its session id was never seen — press s to re-assign"
                        .into();
                }
            }
        };
        match self.runtime.spawn_session(
            &agent_id,
            &cwd,
            Some(crate::spawn::SessionTarget::Resume(sid)),
            None,
            None,
            false,
        ) {
            Ok(tmux) => {
                self.queue_pending_dispatch(tmux.clone(), PROCEED_PROMPT.to_string());
                if let Err(e) = self.tasks.board.rebind_tmux(&id, &tmux) {
                    let _ = self.runtime.kill_session(&tmux);
                    return format!("proceed cancelled: task binding write failed: {e}");
                }
                if let Err(e) = self.tasks.board.set_status(&id, TaskStatus::Running) {
                    return format!("agent resumed but task state write failed: {e}");
                }
                self.focus_task(&id);
                format!("proceeding: {} — agent resumed [{}]", preview, tmux)
            }
            Err(e) => format!("proceed failed: resume error: {}", e),
        }
    }

    /// Flip the focused task between Done and To-Do (an In Progress task
    /// goes to Done — finishing an agent task by hand is always allowed).
    /// Completing a task closes its live agent session; the binding
    /// (`tmux`/`session_id`) is kept so `f` on the Done card can still
    /// resume the transcript. Returns a status line describing the move,
    /// or `None` when no task is focused.
    pub fn toggle_task_done(&mut self) -> Option<String> {
        let t = self.selected_board_task()?;
        let id = t.task_id.clone();
        let preview = card_preview(t);
        let tmux = t.tmux.clone();
        if t.status == TaskStatus::Done {
            if let Err(e) = self.tasks.board.set_status(&id, TaskStatus::Backlog) {
                return Some(format!("reopen failed: {e}"));
            }
            self.tasks.clamp_row();
            return Some(format!("reopened: {}", preview));
        }
        Some(self.finish_task(&id, &preview, tmux.as_deref()))
    }

    /// The question the card still carries, if the first Done on it should
    /// be refused. A card whose newest note is a `Waiting:`/`Needs you:` —
    /// or whose router left a clarification unanswered — is a card an agent
    /// stopped on: closing it records a task nobody did as a task done.
    /// Answering is a note away (`p`, or a `Decided:` note from a session),
    /// and pressing Done again closes it regardless.
    fn unanswered_ask(&mut self, id: &str) -> Option<String> {
        if self.done_refused_on.as_deref() == Some(id) {
            self.done_refused_on = None;
            return None;
        }
        let label = crate::tasks::activity::label(id)
            .filter(|l| l.errand == crate::tasks::activity::Errand::Answer)?;
        self.done_refused_on = Some(id.to_string());
        Some(format!(
            "not done — {} · answer it on the card, or press again to close it anyway",
            label.text
        ))
    }

    /// Mark `id` Done and close its live agent session; the binding is kept
    /// so `f` on the Done card can still resume the transcript. Shared by
    /// Space (toggle) and the manual column move (`L` into Done).
    fn finish_task(&mut self, id: &str, preview: &str, tmux: Option<&str>) -> String {
        if let Some(refusal) = self.unanswered_ask(id) {
            return refusal;
        }
        if let Err(e) = self.tasks.board.set_status(id, TaskStatus::Done) {
            return format!("finish failed: {e}");
        }
        self.tasks.clamp_row();
        let live = tmux.filter(|n| self.runtime.session_exists(n));
        match live {
            Some(name) => match self.runtime.kill_session(name) {
                Ok(()) => format!("done: {} — closed agent session [{}]", preview, name),
                Err(e) => format!(
                    "done: {} — closing agent session [{}] failed: {}",
                    preview, name, e
                ),
            },
            None => format!("done: {}", preview),
        }
    }

    /// `H`/`L`: move the focused card one column left (`dir < 0`) or right
    /// by hand. Planning is agent-owned — assignment is the only way in —
    /// so manual moves hop over it: To-Do ↔ In Progress ↔ Review ↔ Done. A
    /// Planning card can still be moved out: left parks it back in To-Do,
    /// right takes it to In Progress *without* telling the agent to proceed
    /// (Space stays the approve path). Review is normally reached by the
    /// card's own `PR:` note; by hand it is the column before Done, and a
    /// Done card reopens into it. Moving into Done closes the live agent
    /// session exactly like Space. The cursor rides with the card. Returns
    /// `None` when no task is focused or the move runs off the board's edge.
    pub fn move_selected_task(&mut self, dir: i8) -> Option<String> {
        let t = self.selected_board_task()?;
        let id = t.task_id.clone();
        let preview = card_preview(t);
        let tmux = t.tmux.clone();
        let to = match (t.status, dir < 0) {
            (TaskStatus::Backlog, false) => TaskStatus::Running,
            (TaskStatus::Planning, false) => TaskStatus::Running,
            (TaskStatus::Running, false) => TaskStatus::Review,
            (TaskStatus::Review, false) => TaskStatus::Done,
            (TaskStatus::Planning, true) => TaskStatus::Backlog,
            (TaskStatus::Running, true) => TaskStatus::Backlog,
            (TaskStatus::Review, true) => TaskStatus::Running,
            (TaskStatus::Done, true) => TaskStatus::Review,
            (TaskStatus::Backlog, true) | (TaskStatus::Done, false) => return None,
        };
        if to == TaskStatus::Done {
            let msg = self.finish_task(&id, &preview, tmux.as_deref());
            self.focus_task(&id);
            return Some(msg);
        }
        let from_planning = t.status == TaskStatus::Planning;
        if let Err(e) = self.tasks.board.set_status(&id, to) {
            return Some(format!("move failed: {e}"));
        }
        self.focus_task(&id);
        self.tasks.clamp_row();
        let label = match to {
            TaskStatus::Backlog => "To-Do",
            TaskStatus::Running => "In Progress",
            TaskStatus::Review => "Review",
            _ => unreachable!("manual moves only land in To-Do/In Progress/Review here"),
        };
        Some(if from_planning && to == TaskStatus::Running {
            format!(
                "moved: {} → {} — agent not told to proceed (Space does that)",
                preview, label
            )
        } else {
            format!("moved: {} → {}", preview, label)
        })
    }

    /// `1`–`4` on a focused task: set its priority. Priority is the column's
    /// primary sort key, so the card may jump to a new row; the cursor rides
    /// with it (resolved by id through [`Self::focus_task`]). Returns a status
    /// line, or `None` when no task is focused.
    pub fn set_selected_task_priority(&mut self, priority: TaskPriority) -> Option<String> {
        let t = self.selected_board_task()?;
        let id = t.task_id.clone();
        let preview = card_preview(t);
        if let Err(e) = self.tasks.board.set_priority(&id, priority) {
            return Some(format!("priority update failed: {e}"));
        }
        self.focus_task(&id);
        Some(format!("{} · {}", priority.label(), preview))
    }

    /// Delete the focused task. The bound agent session (if any) is left
    /// running — it still shows on the Sessions tab. The task lands in the
    /// undo slot (`u`) and the on-disk archive. Returns the status line.
    pub fn delete_selected_task(&mut self) -> Option<String> {
        let id = self.selected_board_task()?.task_id.clone();
        let removed = match self.tasks.board.remove(&id) {
            Ok(removed) => removed?,
            Err(e) => return Some(format!("delete failed: {e}")),
        };
        self.tasks.clamp_row();
        let preview = card_preview(&removed);
        let tmux = removed.tmux.clone();
        self.tasks.undo = Some(vec![removed]);
        Some(match tmux {
            Some(tmux) => format!(
                "deleted: {} (u undoes) — agent session [{}] left running (close it from Sessions)",
                preview, tmux
            ),
            None => format!("deleted: {} (u undoes)", preview),
        })
    }

    pub fn clear_done_tasks(&mut self) {
        let removed = match self.tasks.board.clear_done() {
            Ok(removed) => removed,
            Err(e) => {
                self.set_status(format!("clear done failed: {e}"));
                return;
            }
        };
        self.tasks.clamp_row();
        if removed.is_empty() {
            self.set_status("no done tasks to clear".to_string());
        } else {
            self.set_status(format!(
                "cleared {} done task{} (u undoes)",
                removed.len(),
                if removed.len() == 1 { "" } else { "s" }
            ));
            self.tasks.undo = Some(removed);
        }
    }

    /// `u`: restore the last `x`/`c` removal (one batch deep). Tasks whose
    /// id somehow returned to the board (hand edit) are skipped. The cursor
    /// jumps to the first restored card. Returns `None` when there is
    /// nothing to undo.
    pub fn undo_task_delete(&mut self) -> Option<String> {
        let batch = self.tasks.undo.take()?;
        let mut first: Option<String> = None;
        let mut restored = 0usize;
        for item in batch {
            let id = item.task_id.clone();
            match self.tasks.board.restore(item) {
                Ok(true) => {
                    restored += 1;
                    first.get_or_insert(id);
                }
                Ok(false) => {}
                Err(e) => {
                    self.set_status(format!("undo failed: {e}"));
                    return None;
                }
            }
        }
        if restored == 0 {
            return Some("nothing to restore — the tasks are already back".into());
        }
        if let Some(id) = first {
            self.focus_task(&id);
        }
        Some(format!(
            "restored {} task{}",
            restored,
            if restored == 1 { "" } else { "s" }
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::agent_runtime::testing::RecordingRuntime;
    use crate::test_util::with_temp_home;
    use std::sync::Arc;

    #[test]
    fn task_controller_uses_runtime_boundary_to_proceed() {
        crate::test_util::with_temp_home(|| {
            let runtime = Arc::new(RecordingRuntime::default());
            let mut app = App::new_with_runtime(runtime.clone());
            let id = app.tasks.board.add("implement it").unwrap().unwrap();
            app.tasks
                .board
                .assign(&id, "/tmp", "claude", "mux-task")
                .unwrap();
            app.focus_task(&id);

            let message = app.proceed_selected_task();

            assert!(message.contains("agent told to implement"));
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Running
            );
            assert_eq!(
                runtime.prompts.lock().unwrap().as_slice(),
                &[("mux-task".into(), PROCEED_PROMPT.into())]
            );
        });
    }

    #[test]
    fn space_on_planning_without_session_stays_planning() {
        with_temp_home(|| {
            let mut app = App::new();
            let id = app.tasks.board.add("plan me").unwrap().unwrap();
            app.tasks
                .board
                .assign(&id, "/tmp", "claude", "mux-dead")
                .unwrap();
            app.focus_task(&id);
            // No live mux and no resolved session id: nothing to deliver
            // the proceed prompt to, so the card must not move — an In
            // Progress card with no agent working it would be a lie.
            let msg = app.task_space_action().unwrap();
            assert!(msg.contains("press s to re-assign"), "msg: {msg}");
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Planning
            );
        });
    }

    #[test]
    fn raising_priority_moves_card_and_cursor_follows() {
        with_temp_home(|| {
            let mut app = App::new();
            app.tasks.board.add("a").unwrap().unwrap();
            app.tasks.board.add("b").unwrap().unwrap();
            let c = app.tasks.board.add("c").unwrap().unwrap();
            app.focus_task(&c);
            assert_eq!((app.tasks.col, app.tasks.row), (0, 2));

            // Bumping c to P1 floats it to the top; the cursor rides along.
            app.set_selected_task_priority(TaskPriority::P1);
            assert_eq!((app.tasks.col, app.tasks.row), (0, 0));
            assert_eq!(app.selected_board_task().unwrap().task_id, c);
        });
    }

    #[test]
    fn toggle_done_completes_assigned_task_and_keeps_binding() {
        with_temp_home(|| {
            let mut app = App::new();
            let id = app.tasks.board.add("ship it").unwrap().unwrap();
            app.tasks
                .board
                .assign(&id, "/tmp", "claude", "mux-dead")
                .unwrap();
            app.focus_task(&id);
            // No live mux session in the test env: the close is skipped
            // and the status stays the plain "done" line.
            assert_eq!(app.toggle_task_done().unwrap(), "done: ship it");
            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.status, TaskStatus::Done);
            // The binding survives completion so `f` can still resume.
            assert_eq!(t.tmux.as_deref(), Some("mux-dead"));
        });
    }

    /// An agent asked and nobody answered: the first Done must not close
    /// the card on the question.
    #[test]
    fn done_on_an_unanswered_ask_is_refused_once() {
        with_temp_home(|| {
            let mut app = App::new();
            let id = app
                .tasks
                .board
                .add("marketplace duplication")
                .unwrap()
                .unwrap();
            let note = |text: &str| {
                crate::ops::task::task_artifact_add_text(&id, text, "test").unwrap();
            };
            app.focus_task(&id);
            note("Needs you: keep the duplicate or fold it in?");

            let refusal = app.toggle_task_done().unwrap();
            assert!(refusal.starts_with("not done"), "{refusal}");
            assert!(refusal.contains("keep the duplicate"), "{refusal}");
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Backlog
            );

            // Pressing again is the user saying it anyway.
            assert_eq!(
                app.toggle_task_done().unwrap(),
                "done: marketplace duplication"
            );
            assert_eq!(app.tasks.board.get(&id).unwrap().status, TaskStatus::Done);
        });
    }

    /// The answer is a note, so an answered card closes on the first
    /// press — and saying you asked is not answering.
    #[test]
    fn an_answered_ask_closes_on_the_first_press() {
        with_temp_home(|| {
            let mut app = App::new();
            let note = |id: &str, text: &str| {
                crate::ops::task::task_artifact_add_text(id, text, "test").unwrap();
            };
            let told = app.tasks.board.add("node 24").unwrap().unwrap();
            note(&told, "Needs you: pin 24 or stay on 22?");
            note(&told, "Posted: asked by DM");
            app.focus_task(&told);
            assert!(app.toggle_task_done().unwrap().starts_with("not done"));

            let answered = app.tasks.board.add("node 22").unwrap().unwrap();
            note(&answered, "Needs you: pin 24 or stay on 22?");
            note(&answered, "Decided: pin 24");
            app.focus_task(&answered);
            assert_eq!(app.toggle_task_done().unwrap(), "done: node 22");
            assert_eq!(
                app.tasks.board.get(&answered).unwrap().status,
                TaskStatus::Done
            );
        });
    }

    #[test]
    fn l_walks_todo_to_done_and_h_back_skipping_planning() {
        with_temp_home(|| {
            let mut app = App::new();
            let id = app.tasks.board.add("hands-on work").unwrap().unwrap();
            app.focus_task(&id);
            // Right: To-Do → In Progress (Planning is agent-owned, so
            // the manual move hops over it).
            let msg = app.move_selected_task(1).unwrap();
            assert!(msg.contains("In Progress"), "msg: {msg}");
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Running
            );
            // The cursor rides with the card.
            assert_eq!(app.selected_board_task().unwrap().task_id, id);
            // Right again: → Review, the column a `PR:` note fills.
            let msg = app.move_selected_task(1).unwrap();
            assert!(msg.contains("Review"), "msg: {msg}");
            assert_eq!(app.tasks.board.get(&id).unwrap().status, TaskStatus::Review);
            // Right again: → Done, stamping done_at exactly like Space.
            let msg = app.move_selected_task(1).unwrap();
            assert!(msg.starts_with("done:"), "msg: {msg}");
            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.status, TaskStatus::Done);
            assert!(t.done_at.is_some());
            // Off the right edge: refused.
            app.focus_task(&id);
            assert!(app.move_selected_task(1).is_none());
            // Left: Done → Review reopens (done_at cleared).
            app.move_selected_task(-1).unwrap();
            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.status, TaskStatus::Review);
            assert!(t.done_at.is_none());
            // Left again: → In Progress, → To-Do; then off the edge.
            app.move_selected_task(-1).unwrap();
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Running
            );
            app.move_selected_task(-1).unwrap();
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Backlog
            );
            assert!(app.move_selected_task(-1).is_none());
        });
    }

    #[test]
    fn planning_card_moves_out_without_proceed_prompt() {
        with_temp_home(|| {
            let mut app = App::new();
            let id = app.tasks.board.add("agent task").unwrap().unwrap();
            app.tasks
                .board
                .assign(&id, "/tmp", "claude", "mux-x")
                .unwrap();
            app.focus_task(&id);
            // Right: Planning → In Progress, but the agent is NOT told
            // to proceed — approving stays Space's job.
            let msg = app.move_selected_task(1).unwrap();
            assert!(msg.contains("not told to proceed"), "msg: {msg}");
            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.status, TaskStatus::Running);
            assert_eq!(t.tmux.as_deref(), Some("mux-x"));
            // A Planning card can also be parked back in To-Do.
            app.tasks
                .board
                .set_status(&id, TaskStatus::Planning)
                .unwrap();
            app.focus_task(&id);
            app.move_selected_task(-1).unwrap();
            assert_eq!(
                app.tasks.board.get(&id).unwrap().status,
                TaskStatus::Backlog
            );
        });
    }

    #[test]
    fn undo_restores_deleted_task_once() {
        with_temp_home(|| {
            let mut app = App::new();
            let id = app.tasks.board.add("precious").unwrap().unwrap();
            app.focus_task(&id);
            let msg = app.delete_selected_task().unwrap();
            assert!(msg.contains("u undoes"), "msg: {msg}");
            assert!(app.tasks.board.get(&id).is_none());
            assert_eq!(app.undo_task_delete().unwrap(), "restored 1 task");
            assert_eq!(app.tasks.board.get(&id).unwrap().prompt, "precious");
            // The slot is one batch deep: a second undo finds nothing.
            assert!(app.undo_task_delete().is_none());
        });
    }

    #[test]
    fn undo_restores_cleared_done_batch_with_statuses() {
        with_temp_home(|| {
            let mut app = App::new();
            let a = app.tasks.board.add("a").unwrap().unwrap();
            let b = app.tasks.board.add("b").unwrap().unwrap();
            app.tasks.board.set_status(&a, TaskStatus::Done).unwrap();
            app.tasks.board.set_status(&b, TaskStatus::Done).unwrap();
            app.clear_done_tasks();
            assert!(app.tasks.board.tasks().is_empty());
            assert_eq!(app.undo_task_delete().unwrap(), "restored 2 tasks");
            // They come back Done, not To-Do — undo is not a reopen.
            assert_eq!(app.tasks.board.get(&a).unwrap().status, TaskStatus::Done);
            assert_eq!(app.tasks.board.get(&b).unwrap().status, TaskStatus::Done);
        });
    }
}
