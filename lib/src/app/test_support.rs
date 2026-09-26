//! Fixtures shared by the app test modules.
//!
//! Most app tests are unix-only: they redirect `$HOME` with
//! `with_temp_home`, which `dirs::home_dir()` honours on unix and ignores on
//! Windows. Every `App` touches the on-disk task store, so one built outside
//! `with_temp_home` reads and writes the developer's real `~/.cc-hub`; the
//! fixtures that build one are unix-only.

#[cfg(unix)]
use crate::agent::AgentKind;
#[cfg(unix)]
use crate::agent_runtime::testing::RecordingRuntime;
#[cfg(unix)]
use crate::app::{App, Command, Effect, HarnessCommand, Tab, TasksCommand};
use crate::models::{SessionInfo, SessionState};
#[cfg(unix)]
use std::sync::Arc;

/// A Claude session in `/tmp` whose session id and tmux name are both
/// `tmux`.
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

#[cfg(unix)]
pub(super) fn session(id: &str, state: SessionState, tmux: Option<&str>) -> SessionInfo {
    SessionInfo {
        agent_id: "claude".into(),
        agent_kind: AgentKind::Claude,
        pid: 4242,
        session_id: id.into(),
        cwd: "/tmp/proj".into(),
        project_name: "proj".into(),
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
        tmux_session: tmux.map(str::to_string),
        current_tool: None,
        is_thinking: false,
        context_tokens: None,
        tool_uses_count: 0,
    }
}

#[cfg(unix)]
pub(super) fn app_with(sessions: Vec<SessionInfo>) -> (App, Arc<RecordingRuntime>) {
    let runtime = Arc::new(RecordingRuntime::default());
    let mut app = App::new_with_runtime(runtime.clone());
    // The grid hides Inactive sessions by default; tests select
    // whatever they inject, so make every fixture visible.
    app.sessions.show_inactive = true;
    app.update_sessions(sessions);
    (app, runtime)
}

#[cfg(unix)]
pub(super) fn status(app: &App) -> String {
    app.status_msg
        .as_ref()
        .map(|(m, _)| m.clone())
        .unwrap_or_default()
}

/// An App wired to a recording runtime, with no seeded sessions.
#[cfg(unix)]
pub(super) fn task_app() -> (App, Arc<RecordingRuntime>) {
    let runtime = Arc::new(RecordingRuntime::default());
    let app = App::new_with_runtime(runtime.clone());
    (app, runtime)
}

#[cfg(unix)]
pub(super) fn tasks(app: &mut App, cmd: TasksCommand) -> Vec<Effect> {
    app.execute(Command::Tasks(cmd))
}

#[cfg(unix)]
const AGENT_SPEC: &str = "description = \"Watches PRs\"\n\n[run]\nmax_budget_usd = 0.50  # per run\n\n[prompt]\ninstruction = \"go\"\n";

/// An app whose Agents tab holds one agent, `bb-prs`, on disk under the
/// temp home. Returns the agent's folder.
#[cfg(unix)]
pub(super) fn app_with_agent() -> (App, Arc<RecordingRuntime>, std::path::PathBuf) {
    let (mut app, runtime) = app_with(Vec::new());
    let dir = dirs::home_dir().unwrap().join(".cc-hub/agents/bb-prs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), AGENT_SPEC).unwrap();
    app.harness.update(vec![crate::harness::snapshot(&dir)]);
    app.harness.supervisor_on = true;
    app.current_tab = Tab::Agents;
    (app, runtime, dir)
}

#[cfg(unix)]
pub(super) fn harness(app: &mut App, cmd: HarnessCommand) -> Vec<Effect> {
    app.execute(Command::Harness(cmd))
}
