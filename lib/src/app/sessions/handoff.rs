//! `h`: mark the session whose last reply the next spawn opens with. The
//! mark is taken in [`App::watch_spawn`]; see [`crate::handoff`].

use crate::app::App;
use crate::handoff::Handoff;

impl App {
    /// Mark the selected session for a [`Handoff`], or unmark it when it
    /// already is. Marking another session moves the mark.
    pub fn toggle_handoff(&mut self) {
        let Some(session) = self.selected_session_info().cloned() else {
            return;
        };
        let label = session.label();
        if self.sessions.hands_off(&session.session_id) {
            self.sessions.handoff = None;
            self.set_status(format!("handoff from {label} dropped"));
            return;
        }
        match Handoff::of(&session) {
            Some(handoff) => {
                self.sessions.handoff = Some(handoff);
                self.set_status(format!(
                    "handoff from {label} — the next new session opens with its last reply"
                ));
            }
            None => self.set_status(format!("{label} has no reply to hand off yet")),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use crate::app::test_support::{app_with, fake_session, session, status};
    use crate::app::{Command, DispatchAction, SessionsCommand};
    use crate::models::SessionState;
    use crate::send::Delivery;

    /// A Claude transcript whose last reply is `reply`, behind a tool call
    /// that has no text of its own.
    fn transcript_replying(dir: &std::path::Path, reply: &str) -> std::path::PathBuf {
        let path = dir.join("sid-1.jsonl");
        let lines = [
            serde_json::json!({"type": "user", "message": {"content": "plan it"}}),
            serde_json::json!({"type": "assistant", "message": {"content": [
                {"type": "text", "text": reply}
            ]}}),
            serde_json::json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": "t1", "name": "Read", "input": {}}
            ]}}),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn the_next_new_session_opens_with_the_marked_reply_drafted() {
        crate::test_util::with_temp_home(|| {
            let dir = tempfile::tempdir().unwrap();
            let mut source = session("sid-1", SessionState::Idle, Some("cc-agent-1"));
            source.jsonl_path = Some(transcript_replying(dir.path(), "Here is the plan."));
            let (mut app, _runtime) = app_with(vec![source]);

            app.execute(Command::Sessions(SessionsCommand::ToggleHandoff));
            assert!(app.sessions.hands_off("sid-1"));
            app.execute(Command::Sessions(SessionsCommand::SpawnAgentHere));
            assert!(app.sessions.handoff.is_none(), "the spawn takes the mark");

            app.sessions.last_sessions = vec![fake_session("mock-spawn", SessionState::Idle)];
            match app.poll_pending_dispatch() {
                DispatchAction::Send {
                    tmux,
                    prompt,
                    delivery,
                } => {
                    assert_eq!(tmux, "mock-spawn");
                    assert_eq!(delivery, Delivery::Draft);
                    assert_eq!(prompt, "<context>\nHere is the plan.\n</context>\n\n");
                }
                _ => panic!("the draft should go to the new session"),
            }
        });
    }

    #[test]
    fn a_second_h_drops_the_mark() {
        crate::test_util::with_temp_home(|| {
            let dir = tempfile::tempdir().unwrap();
            let mut source = session("sid-1", SessionState::Idle, Some("cc-agent-1"));
            source.jsonl_path = Some(transcript_replying(dir.path(), "Done."));
            let (mut app, _runtime) = app_with(vec![source]);

            app.execute(Command::Sessions(SessionsCommand::ToggleHandoff));
            app.execute(Command::Sessions(SessionsCommand::ToggleHandoff));
            app.execute(Command::Sessions(SessionsCommand::SpawnAgentHere));

            assert!(app.sessions.handoff.is_none());
            assert_eq!(app.pending_dispatch_count(), 0, "nothing to draft");
        });
    }

    #[test]
    fn a_session_with_no_reply_cannot_be_marked() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _runtime) = app_with(vec![session(
                "sid-1",
                SessionState::Idle,
                Some("cc-agent-1"),
            )]);
            app.execute(Command::Sessions(SessionsCommand::ToggleHandoff));
            assert!(app.sessions.handoff.is_none());
            assert!(status(&app).contains("no reply"), "got: {}", status(&app));
        });
    }
}
