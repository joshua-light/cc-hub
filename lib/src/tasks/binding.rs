use crate::models::{SessionInfo, SessionState};
use crate::tasks::store::TaskState;

/// What a scan teaches a card about its own session.
pub(super) enum Binding {
    /// The card's session is live in another tmux: follow it there.
    FollowSession(String),
    /// The card only knows its tmux; the session running there is its own.
    LearnSession(String),
}

impl Binding {
    pub(super) fn learned(card: &TaskState, sessions: &[SessionInfo]) -> Option<Self> {
        let live = |s: &&SessionInfo| s.state != SessionState::Inactive;
        if let Some(sid) = card.session_id.as_deref() {
            let own = sessions.iter().filter(live).find(|s| s.session_id == sid);
            if let Some(tmux) = own.and_then(|s| s.tmux_session.clone()) {
                return (card.tmux.as_deref() != Some(tmux.as_str()))
                    .then_some(Binding::FollowSession(tmux));
            }
        }
        let tmux = card.tmux.as_deref()?;
        let running = sessions
            .iter()
            .find(|s| s.tmux_session.as_deref() == Some(tmux))?;
        (card.session_id.as_deref() != Some(running.session_id.as_str()))
            .then(|| Binding::LearnSession(running.session_id.clone()))
    }

    pub(super) fn apply(&self, card: &mut TaskState) {
        match self {
            Binding::FollowSession(tmux) => card.tmux = Some(tmux.clone()),
            Binding::LearnSession(sid) => card.session_id = Some(sid.clone()),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::tasks::PersonalBoard;
    use crate::test_util::with_temp_home;

    fn scanned(id: &str, state: SessionState, tmux: &str) -> SessionInfo {
        SessionInfo {
            agent_id: "claude".into(),
            agent_kind: crate::agent::AgentKind::Claude,
            pid: 4242,
            session_id: id.into(),
            cwd: "/tmp/project".into(),
            project_name: "project".into(),
            started_at: 0,
            last_activity: None,
            state,
            last_user_message: None,
            summary: None,
            title: None,
            titling: false,
            model: None,
            git_branch: None,
            version: None,
            jsonl_path: None,
            tmux_session: Some(tmux.into()),
            current_tool: None,
            is_thinking: false,
            context_tokens: None,
            tool_uses_count: 0,
        }
    }

    #[test]
    fn a_card_follows_its_session_to_where_it_runs_now() {
        with_temp_home(|| {
            let mut board = PersonalBoard::load();
            let id = board.add("managed task").unwrap().unwrap();
            board
                .bind_resource(
                    &id,
                    "/tmp/project",
                    "cc-1",
                    "cchub-stale",
                    Some("sid-worker"),
                )
                .unwrap();
            // The card's tmux was repointed at a resumed, long-dead session
            // while the worker it names runs on in the broker's tmux.
            let learned = board
                .bind_sessions(&[
                    scanned("sid-exhausted", SessionState::Idle, "cchub-stale"),
                    scanned("sid-worker", SessionState::Processing, "cchr-worker-2"),
                ])
                .unwrap();
            assert!(learned);
            let card = PersonalBoard::load().get(&id).cloned().unwrap();
            assert_eq!(card.tmux.as_deref(), Some("cchr-worker-2"));
            assert_eq!(card.session_id.as_deref(), Some("sid-worker"));
            // Nothing more to learn: the same scan is a no-op.
            assert!(!board
                .bind_sessions(&[scanned("sid-worker", SessionState::Idle, "cchr-worker-2")])
                .unwrap());
        });
    }

    #[test]
    fn a_card_whose_session_is_gone_learns_the_one_running_in_its_tmux() {
        with_temp_home(|| {
            let mut board = PersonalBoard::load();
            let id = board.add("resumed task").unwrap().unwrap();
            board
                .bind_resource(
                    &id,
                    "/tmp/project",
                    "claude",
                    "cchub-resumed",
                    Some("sid-old"),
                )
                .unwrap();
            // A Claude resume forks a fresh session id in the card's tmux;
            // the old id only survives in the archive.
            board
                .bind_sessions(&[
                    scanned("sid-old", SessionState::Inactive, "cchub-dead"),
                    scanned("sid-forked", SessionState::Idle, "cchub-resumed"),
                ])
                .unwrap();
            let card = PersonalBoard::load().get(&id).cloned().unwrap();
            assert_eq!(card.session_id.as_deref(), Some("sid-forked"));
            assert_eq!(card.tmux.as_deref(), Some("cchub-resumed"));
        });
    }

    #[test]
    fn resource_replacement_changes_session_without_changing_board_status() {
        with_temp_home(|| {
            let mut board = PersonalBoard::load();
            let id = board.add("managed task").unwrap().unwrap();
            let status = board.get(&id).unwrap().status;
            board
                .bind_resource(&id, "/tmp/project", "cc-1", "old", Some("old-session"))
                .unwrap();
            board
                .bind_resource(&id, "/tmp/project", "codex-2", "new", None)
                .unwrap();
            let reloaded = PersonalBoard::load();
            let task = reloaded.get(&id).unwrap();
            assert_eq!(task.status, status);
            assert_eq!(task.agent_id.as_deref(), Some("codex-2"));
            assert_eq!(task.tmux.as_deref(), Some("new"));
            assert_eq!(task.session_id, None);
        });
    }
}
