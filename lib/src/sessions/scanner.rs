//! The live-grid scan: merges every enabled backend's sessions into one
//! stably ordered list, plus the cheap liveness pass between full scans.

use crate::agent::AgentKind;
use crate::config;
use crate::conversation;
use crate::models::{SessionDetail, SessionInfo, SessionState};
use crate::sessions::{claude, codex, pi};
use std::collections::HashSet;
use std::path::PathBuf;

pub(crate) use claude::paths::encode_path;
pub use claude::paths::{find_jsonl, find_jsonl_anywhere, scratch_project_dir_name};

/// Order sessions by liveness bucket, then stable keys — active sessions
/// first, idle after them, inactive last; within a bucket newest first with
/// session id as the deterministic tiebreak.
///
/// The bucket is deliberately coarse: Processing, WaitingForInput and
/// Question all rank equally because they flip between each other every few
/// seconds while agents work, and sorting on those flips made cards swap
/// positions under the cursor on every scan tick (the selection followed the
/// session id to its new slot, so a keypress could advance the selection
/// logically while the highlight visibly stayed put). Falling asleep or
/// waking up is a rare, meaningful transition, so a card moving then is
/// intended, not churn.
pub(super) fn sort_stable(sessions: &mut [SessionInfo]) {
    sessions.sort_by(|a, b| {
        a.state
            .liveness_rank()
            .cmp(&b.state.liveness_rank())
            .then_with(|| b.started_at.cmp(&a.started_at))
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
}

pub fn scan_sessions() -> Vec<SessionInfo> {
    let enabled = config::get().enabled_agent_kinds();
    let mut sessions = Vec::new();
    // Titles are cheap to load and don't change within a scan — read once and
    // hand a reference to every site that builds a SessionInfo.
    let titles = crate::title::load();
    if enabled.contains(&AgentKind::Claude) {
        sessions.extend(claude::scan(&titles));
    }
    if enabled.contains(&AgentKind::Pi) {
        let pi_agents: Vec<_> = config::get()
            .resolved_agents()
            .into_values()
            .filter(|a| a.kind == AgentKind::Pi)
            .collect();
        sessions.extend(pi::scan(&pi_agents, &titles));
    }
    if enabled.contains(&AgentKind::Codex) {
        let codex_agents: Vec<_> = config::get()
            .resolved_agents()
            .into_values()
            .filter(|a| a.kind == AgentKind::Codex)
            .collect();
        sessions.extend(codex::scan(&codex_agents, &titles));
    }

    // The tool-use count cache is shared across Claude and Pi transcripts, so
    // evict it here — after both scans — with every live path this tick.
    // (conversation's caches are Claude-only and are retained inside
    // scan_claude_sessions.) Without this the path-keyed cache grows one entry
    // per JSONL ever scanned, for the whole process lifetime.
    let live_paths: HashSet<PathBuf> = sessions
        .iter()
        .filter_map(|s| s.jsonl_path.clone())
        .collect();
    crate::conversation::tool_count::retain_cached(&live_paths);

    crate::resources::label_sessions(&mut sessions);
    sort_stable(&mut sessions);
    sessions
}

/// True if `pid` is still a live process of `kind`. The liveness rule is
/// per-backend: `is_pid_alive` is Claude-specific (executable path, real
/// parent), so applying it to a Pi session wrongly declares every live `pi`
/// process dead. Pi only needs "alive and still a pi process" — pi children
/// legitimately reparent to init, so the has-real-parent orphan rule must not
/// apply.
fn session_pid_alive(kind: AgentKind, pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    match kind {
        AgentKind::Claude => claude::is_pid_alive(pid),
        // Pi and Codex both need only "alive and still that agent's process" —
        // the Claude has-real-parent orphan rule must not apply (their children
        // legitimately reparent to init).
        AgentKind::Pi | AgentKind::Codex => crate::platform::process::is_agent_process(kind, pid),
    }
}

/// Cheap fallback between full filesystem reconciliations. File watchers drive
/// transcript/state updates; this pass only catches processes that exited
/// without writing another event, avoiding directory walks and tmux snapshots
/// on every short fallback tick.
pub fn refresh_process_liveness(sessions: &mut [SessionInfo]) -> bool {
    let mut changed = false;
    for session in sessions {
        if session.state != SessionState::Inactive
            && !session_pid_alive(session.agent_kind, session.pid)
        {
            session.state = SessionState::Inactive;
            session.tmux_session = None;
            changed = true;
        }
    }
    changed
}

pub fn load_detail(session_id: &str, sessions: &[SessionInfo]) -> Option<SessionDetail> {
    let info = sessions.iter().find(|s| s.session_id == session_id)?;
    match info.agent_kind {
        AgentKind::Claude => {
            let jsonl_path = info.jsonl_path.as_ref()?;
            let entries = conversation::read_jsonl_tail(jsonl_path, 65536);
            let recent_messages = conversation::extract_messages(&entries, 15);
            let (total_input_tokens, total_output_tokens) =
                conversation::extract_token_totals(&entries);
            Some(SessionDetail {
                info: info.clone(),
                recent_messages,
                total_input_tokens,
                total_output_tokens,
            })
        }
        AgentKind::Pi => pi::load_detail(info),
        AgentKind::Codex => codex::load_detail(info),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::process::ProcessInfo;

    fn session(id: &str, started_at: u64, state: SessionState) -> SessionInfo {
        SessionInfo {
            agent_id: "claude".into(),
            agent_kind: crate::agent::AgentKind::Claude,
            pid: 1,
            session_id: id.into(),
            cwd: "/tmp".into(),
            project_name: "tmp".into(),
            started_at,
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
            tmux_session: None,
            current_tool: None,
            is_thinking: false,
            context_tokens: None,
            tool_uses_count: 0,
        }
    }

    // The fallback liveness pass judges each session by its own backend's
    // rule. The flicker regression was `refresh_process_liveness` running the
    // Claude-specific `is_pid_alive` over Pi sessions: a live `pi` process
    // isn't a `claude` binary, so every fallback tick flipped it to Inactive
    // and cleared its tmux (the next full scan restored it → blink). This locks
    // the routing helper: pid 0 is the no-process sentinel for both, and a Pi
    // session is decided by the Pi detector, never the Claude one.
    #[test]
    fn session_pid_alive_routes_by_backend() {
        assert!(!session_pid_alive(AgentKind::Pi, 0));
        assert!(!session_pid_alive(AgentKind::Claude, 0));
        // The test process is a real, live pid that is neither `claude` nor
        // `pi`. Both branches must therefore report it dead — proving each
        // consults its own detector rather than blindly trusting kill(0).
        let live_pid = std::process::id();
        assert!(crate::platform::process::Process::is_alive(live_pid));
        assert!(!session_pid_alive(AgentKind::Claude, live_pid));
        assert!(!session_pid_alive(AgentKind::Pi, live_pid));
    }

    // A live Pi session must not be reaped by the fallback pass. We can't
    // fabricate a real `pi` process in a unit test, so drive the reaping branch
    // instead: a Pi session with the no-process sentinel pid is flipped to
    // Inactive and its tmux cleared, exactly as a genuinely-dead one would be.
    #[test]
    fn refresh_liveness_reaps_dead_pi_session() {
        let mut dead = session("pi-dead", 0, SessionState::Idle);
        dead.agent_kind = AgentKind::Pi;
        dead.pid = 0;
        dead.tmux_session = Some("mux".into());
        let mut list = [dead];
        assert!(refresh_process_liveness(&mut list));
        assert_eq!(list[0].state, SessionState::Inactive);
        assert_eq!(list[0].tmux_session, None);
    }

    #[test]
    fn sort_stable_puts_idle_after_active_and_inactive_last() {
        let mut sessions = vec![
            session("idle-new", 400, SessionState::Idle),
            session("inactive", 500, SessionState::Inactive),
            session("processing-old", 100, SessionState::Processing),
            session("idle-old", 200, SessionState::Idle),
            session("waiting", 300, SessionState::WaitingForInput),
        ];
        sort_stable(&mut sessions);
        let order: Vec<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
        // Active bucket first (newest first), then idle (newest first),
        // inactive dead last regardless of recency.
        assert_eq!(
            order,
            vec![
                "waiting",
                "processing-old",
                "idle-new",
                "idle-old",
                "inactive"
            ]
        );
    }

    #[test]
    fn sort_stable_ranks_all_active_flavors_equally() {
        // Processing / WaitingForInput / Question must not order against each
        // other by state — only by the stable keys — so the rapid flips
        // between them can't reshuffle cards.
        let mut sessions = vec![
            session("question", 100, SessionState::Question),
            session("processing", 300, SessionState::Processing),
            session("waiting", 200, SessionState::WaitingForInput),
        ];
        sort_stable(&mut sessions);
        let order: Vec<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(order, vec!["processing", "waiting", "question"]);
    }
}
