//! Fixtures shared by the app test modules.

use crate::models::{SessionInfo, SessionState};

pub(super) fn fake_session(tmux: &str, state: SessionState) -> SessionInfo {
    SessionInfo {
        agent_id: "claude".into(),
        agent_kind: crate::agent::AgentKind::Claude,
        pid: 1,
        session_id: tmux.into(),
        cwd: "/tmp".into(),
        project_name: "tmp".into(),
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
