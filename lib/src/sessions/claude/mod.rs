//! Claude Code session discovery: live sessions from `~/.claude/sessions/*.json`,
//! resolved to their transcripts, plus recent orphan transcripts as Inactive.
//!
//! - [`paths`] — the Claude home layout and transcript lookup.
//! - [`clear_chain`] — follows `/clear` forks to the live transcript.
//! - [`inactive`] — the orphan-transcript walk for Inactive cards.

mod clear_chain;
mod inactive;
pub(super) mod paths;

use crate::agent::AgentKind;
use crate::config;
use crate::conversation;
use crate::models::{short_sid, RawSession, SessionInfo, SessionState};
use crate::platform::process::{Process, ProcessInfo};
use clear_chain::{read_clears_from_history, resolve_jsonl_paths, ClearMap};
use inactive::scan_orphan_jsonls;
use log::{debug, info, warn};
use paths::{claude_dir, is_scratch_cwd, sessions_dir};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Seconds since `path` was last modified, or `None` if stat fails.
fn mtime_age_secs(path: &Path) -> Option<u64> {
    path.metadata()
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()
        .map(|d| d.as_secs())
}

/// Check if a process has a real parent (not reparented to init).
fn has_real_parent(pid: u32) -> bool {
    Process::parent_pid(pid).is_some_and(|ppid| ppid > 1)
}

pub(super) fn is_pid_alive(pid: u32) -> bool {
    // Check that the process exists AND is actually a claude process.
    // This avoids false positives from PID reuse (another process gets the
    // same PID after claude exits).
    if !Process::is_alive(pid) {
        debug!("pid {} not alive (kill(0) failed)", pid);
        return false;
    }
    if !crate::platform::process::is_agent_process(AgentKind::Claude, pid) {
        debug!(
            "pid {} alive but not claude (name={})",
            pid,
            Process::name(pid)
        );
        return false;
    }
    // A claude process reparented to init (ppid=1) is an orphan from a
    // killed terminal — it will never receive input again, so treat it
    // as dead.
    if !has_real_parent(pid) {
        debug!("pid {} is claude but orphaned (reparented to init)", pid);
        return false;
    }
    true
}

fn read_raw_sessions() -> Vec<RawSession> {
    let dir = match sessions_dir() {
        Some(d) => d,
        None => {
            warn!("sessions dir not found");
            return Vec::new();
        }
    };

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            warn!("cannot read sessions dir: {}", e);
            return Vec::new();
        }
    };

    // Collect all sessions, then deduplicate by session_id.
    // When a session is resumed, Claude Code creates a new file with the same
    // session_id but a different PID. We keep the entry whose process is still
    // alive, or the most recently started one if both are dead.
    let mut by_session_id = HashMap::<String, RawSession>::new();
    let mut file_count = 0u32;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        file_count += 1;
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(raw) = serde_json::from_str::<RawSession>(&contents) {
                // Skip the one-shot `cc-hub-new -p` children our titler
                // spawns — they live briefly in scratch_cwd and would
                // otherwise surface as a spurious "cc-hub-summaries"
                // project in the grid.
                if is_scratch_cwd(&raw.cwd) {
                    debug!(
                        "session file: skipping scratch-cwd sid={} pid={}",
                        short_sid(&raw.session_id),
                        raw.pid
                    );
                    continue;
                }
                debug!(
                    "session file: pid={} sid={} cwd={}",
                    raw.pid,
                    short_sid(&raw.session_id),
                    raw.cwd
                );
                let keep_existing = match by_session_id.get(&raw.session_id) {
                    Some(existing) => {
                        let existing_alive = is_pid_alive(existing.pid);
                        let new_alive = is_pid_alive(raw.pid);
                        let result = match (existing_alive, new_alive) {
                            (false, true) => false, // new is alive, replace
                            (true, false) => true,  // existing is alive, skip
                            // Both alive: prefer the one with a real parent
                            // (i.e. still attached to a terminal window).
                            (true, true) => {
                                match (has_real_parent(existing.pid), has_real_parent(raw.pid)) {
                                    (true, false) => true,
                                    (false, true) => false,
                                    _ => existing.started_at >= raw.started_at,
                                }
                            }
                            // Both dead: keep the most recently started.
                            (false, false) => existing.started_at >= raw.started_at,
                        };
                        debug!(
                            "dedup sid={}: existing pid={} alive={} vs new pid={} alive={} → {}",
                            short_sid(&raw.session_id),
                            existing.pid,
                            existing_alive,
                            raw.pid,
                            new_alive,
                            if result { "keep existing" } else { "replace" }
                        );
                        result
                    }
                    None => false,
                };
                if !keep_existing {
                    by_session_id.insert(raw.session_id.clone(), raw);
                }
            }
        }
    }

    debug!(
        "read_raw_sessions: {} files, {} unique sessions",
        file_count,
        by_session_id.len()
    );
    by_session_id.into_values().collect()
}

/// Reconcile the transcript-derived state with the live `status` Claude Code
/// writes to the session file.
///
/// When the agent blocks on an interactive prompt (AskUserQuestion, permission
/// prompt, plan review) it sets `status="waiting"` immediately — but the
/// prompt's `tool_use` block isn't flushed to the JSONL transcript until the
/// user answers it. So while the prompt is open, the transcript's last
/// meaningful entry is the *previous* (already-resolved) turn, and
/// [`conversation::extract_state`] reads it as `Processing`. That's why a card
/// only turned blue *after* the user answered.
///
/// Trusting the session file's `waiting` status lets us surface the blue
/// `Question` state while the prompt is actually open. We can't tell *which*
/// kind of prompt it is (the session file labels even AskUserQuestion as a
/// "permission prompt"), so every live `waiting` session shows as `Question`.
/// Only `waiting` is overridden; every other status defers to the transcript,
/// which handles the many edge cases (interrupts, thinking, sub-agents, …).
fn reconcile_with_session_status(
    transcript_state: SessionState,
    is_alive: bool,
    status: Option<&str>,
) -> SessionState {
    if is_alive && status == Some("waiting") {
        SessionState::Question
    } else {
        transcript_state
    }
}

/// Extracted JSONL data for a session, avoiding a 7-element tuple.
struct JsonlData {
    state: SessionState,
    last_user_message: Option<String>,
    last_activity: Option<u64>,
    git_branch: Option<String>,
    model: Option<String>,
    version: Option<String>,
    summary: Option<String>,
    current_tool: Option<conversation::CurrentTool>,
    is_thinking: bool,
    context_tokens: Option<u64>,
    tool_uses_count: u64,
}

fn project_name(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Claude sessions under the default home and every extra Claude account home.
pub(super) fn scan(titles: &HashMap<String, String>) -> Vec<SessionInfo> {
    let mut sessions = scan_claude_sessions(titles);
    let default_home = claude_dir();
    let mut seen = HashSet::new();
    for account in crate::resources::accounts()
        .values()
        .filter(|a| a.provider == AgentKind::Claude)
    {
        if let Some(home) = account.home() {
            if Some(&home) != default_home.as_ref() && seen.insert(home.clone()) {
                sessions.extend(crate::resources::with_claude_home(home, || {
                    scan_claude_sessions(titles)
                }));
            }
        }
    }
    sessions
}

fn scan_claude_sessions(titles: &HashMap<String, String>) -> Vec<SessionInfo> {
    let raw_sessions = read_raw_sessions();
    let clears = read_clears_from_history();
    // `clears` is a cached Arc; borrow the map for the rest of this scan.
    let clears: &ClearMap = &clears;
    // Derive claimed session IDs from the sessions we already read,
    // avoiding a redundant second pass over the session metadata files.
    let claimed: HashSet<String> = raw_sessions.iter().map(|r| r.session_id.clone()).collect();

    debug!(
        "scan_sessions: {} raw sessions, {} clears, {} claimed ids",
        raw_sessions.len(),
        clears.len(),
        claimed.len()
    );

    let alive_by_pid: HashMap<u32, bool> = raw_sessions
        .iter()
        .map(|r| (r.pid, is_pid_alive(r.pid)))
        .collect();
    let alive_count = alive_by_pid.values().filter(|&&v| v).count();

    info!(
        "scan_sessions: {} raw → {} alive, {} dead (subject to inactive window)",
        raw_sessions.len(),
        alive_count,
        raw_sessions.len() - alive_count
    );

    let jsonl_map = resolve_jsonl_paths(&raw_sessions, clears, &claimed);

    // Snapshot tmux once per scan so we can tag each session with its hosting
    // tmux session name (if any) without reshelling per pid.
    let tmux_panes = crate::send::tmux_panes();

    let inactive_window_secs = config::get().inactive.window_secs;
    let mut sessions: Vec<SessionInfo> = raw_sessions
        .into_iter()
        .filter_map(|raw| {
            let is_alive = alive_by_pid.get(&raw.pid).copied().unwrap_or(false);
            let jsonl_path = jsonl_map.get(&raw.session_id).cloned().flatten();
            if !is_alive {
                let within_window = jsonl_path
                    .as_deref()
                    .and_then(mtime_age_secs)
                    .is_some_and(|s| s <= inactive_window_secs);
                if !within_window {
                    return None;
                }
            }
            Some((raw, is_alive, jsonl_path))
        })
        .map(|(raw, is_alive, jsonl_path)| {
            let sid_short = short_sid(&raw.session_id);

            let was_cleared = clears.contains_key(&raw.session_id);
            debug!(
                "pid={} sid={} cwd={} alive={} cleared={} jsonl={}",
                raw.pid,
                sid_short,
                raw.cwd,
                is_alive,
                was_cleared,
                jsonl_path
                    .as_ref()
                    .map_or("none".to_string(), |p| p.display().to_string())
            );

            let mut data = match &jsonl_path {
                Some(path) => {
                    // Memoized on (path, mtime): skips the whole tail-read +
                    // extract pipeline when the JSONL hasn't changed this tick.
                    let derived = conversation::derive_state_cached(path)
                        .map(|d| (*d).clone())
                        .unwrap_or_else(|| conversation::StateDerivation {
                            state: SessionState::Idle,
                            last_user_message: None,
                            last_activity: None,
                            git_branch: None,
                            model: None,
                            version: None,
                            current_tool: None,
                            is_thinking: false,
                            context_tokens: None,
                        });
                    let mtime_age_secs = mtime_age_secs(path);
                    let mut state = derived.state;
                    // First user message is immutable per session — cached
                    // permanently, so the head is read at most once per path.
                    let summary = conversation::first_user_message_cached(path);
                    let tool_uses_count = crate::conversation::tool_count::count_claude(path);

                    debug!(
                        "  sid={} raw_state={} last_activity={:?}",
                        sid_short, state, derived.last_activity
                    );

                    // If the JSONL was modified very recently but state
                    // reads as Idle, the assistant likely hasn't written
                    // its first response yet (e.g. right after a slash
                    // command). Upgrade to Processing.
                    if is_alive
                        && state == SessionState::Idle
                        && mtime_age_secs.is_some_and(|s| s < 30)
                    {
                        debug!(
                            "  sid={} upgrading Idle→Processing (mtime age={}s)",
                            sid_short,
                            mtime_age_secs.unwrap()
                        );
                        state = SessionState::Processing;
                    }

                    debug!(
                        "  sid={} final_state={} model={:?} branch={:?}",
                        sid_short, state, derived.model, derived.git_branch
                    );

                    JsonlData {
                        state,
                        last_user_message: derived.last_user_message,
                        last_activity: derived.last_activity,
                        git_branch: derived.git_branch,
                        model: derived.model,
                        version: derived.version,
                        summary,
                        current_tool: derived.current_tool,
                        is_thinking: derived.is_thinking,
                        context_tokens: derived.context_tokens,
                        tool_uses_count,
                    }
                }
                None => {
                    debug!("  sid={} no jsonl → Idle", sid_short);
                    JsonlData {
                        state: SessionState::Idle,
                        last_user_message: None,
                        last_activity: None,
                        git_branch: None,
                        model: None,
                        version: None,
                        summary: None,
                        current_tool: None,
                        is_thinking: false,
                        context_tokens: None,
                        tool_uses_count: 0,
                    }
                }
            };

            // The transcript can't see a prompt that's open but not yet
            // answered; the session file's live status can. Surface `waiting`
            // as the blue Question state while the prompt is open.
            let reconciled =
                reconcile_with_session_status(data.state.clone(), is_alive, raw.status.as_deref());
            if reconciled != data.state {
                debug!(
                    "  sid={} status={:?} overrides {} → {}",
                    sid_short, raw.status, data.state, reconciled
                );
                data.state = reconciled;
            }

            if !is_alive {
                data.state = SessionState::Inactive;
            }

            let tmux_session = if is_alive {
                crate::send::tmux_session_for_pid_in(raw.pid, &tmux_panes)
            } else {
                None
            };

            let title = titles.get(&raw.session_id).cloned();

            SessionInfo {
                agent_id: "claude".into(),
                agent_kind: AgentKind::Claude,
                pid: raw.pid,
                session_id: raw.session_id,
                project_name: project_name(&raw.cwd),
                cwd: raw.cwd,
                started_at: raw.started_at,
                last_activity: data.last_activity,
                state: data.state,
                last_user_message: data.last_user_message,
                summary: data.summary,
                title,
                model: data.model,
                git_branch: data.git_branch,
                version: data.version,
                jsonl_path,
                tmux_session,
                current_tool: data.current_tool,
                is_thinking: data.is_thinking,
                titling: false,
                context_tokens: data.context_tokens,
                tool_uses_count: data.tool_uses_count,
            }
        })
        .collect();

    let claimed_paths: HashSet<PathBuf> = sessions
        .iter()
        .filter_map(|s| s.jsonl_path.clone())
        .collect();
    let (orphans, total_in_window) = scan_orphan_jsonls(&claimed_paths, titles);
    info!(
        "scan_sessions: {} from metadata + {} orphan JSONLs (of {} within window, capped at {} per project)",
        sessions.len(),
        orphans.len(),
        total_in_window,
        config::get().inactive.max_per_project,
    );
    sessions.extend(orphans);

    // Evict derived-state / summary cache entries for transcripts that didn't
    // surface this scan (sessions aged out of the window, deleted JSONLs), so
    // the caches don't grow unbounded.
    let visited: HashSet<PathBuf> = sessions
        .iter()
        .filter_map(|s| s.jsonl_path.clone())
        .collect();
    conversation::retain_cached(&visited);

    super::scanner::sort_stable(&mut sessions);

    sessions
}

#[cfg(test)]
mod tests {
    use super::*;

    // While an AskUserQuestion / permission prompt is open, Claude Code sets
    // the session file's status to "waiting" but hasn't yet written the
    // prompt's tool_use to the transcript, so the transcript reads as
    // Processing. A live "waiting" session must show as Question (blue) anyway.
    #[test]
    fn live_waiting_status_overrides_processing_to_question() {
        assert_eq!(
            reconcile_with_session_status(SessionState::Processing, true, Some("waiting")),
            SessionState::Question
        );
    }

    // Other statuses defer entirely to the transcript-derived state.
    #[test]
    fn non_waiting_status_defers_to_transcript_state() {
        for status in [Some("busy"), Some("idle"), None] {
            assert_eq!(
                reconcile_with_session_status(SessionState::Processing, true, status),
                SessionState::Processing
            );
            assert_eq!(
                reconcile_with_session_status(SessionState::WaitingForInput, true, status),
                SessionState::WaitingForInput
            );
        }
    }

    // A dead process's stale "waiting" status must not resurrect it as a
    // pending question — liveness gates the override.
    #[test]
    fn dead_session_waiting_status_is_not_question() {
        assert_eq!(
            reconcile_with_session_status(SessionState::Processing, false, Some("waiting")),
            SessionState::Processing
        );
    }

    // Guard the serde wiring: the override is only reachable if `status`
    // actually deserializes off the real session-file shape Claude Code writes.
    #[test]
    fn raw_session_deserializes_waiting_status() {
        let json = r#"{"pid":61091,"sessionId":"1e391030","cwd":"/x","startedAt":1780046464303,
            "procStart":"Fri May 29 09:21:03 2026","version":"2.1.156","peerProtocol":1,
            "kind":"interactive","entrypoint":"cli","status":"waiting","updatedAt":1780048108142,
            "waitingFor":"permission prompt"}"#;
        let raw: RawSession = serde_json::from_str(json).expect("deserialize");
        assert_eq!(raw.status.as_deref(), Some("waiting"));
    }

    // Older clients omit `status` entirely — must still deserialize, leaving
    // the transcript as the sole source of truth.
    #[test]
    fn raw_session_without_status_defaults_to_none() {
        let json = r#"{"pid":1,"sessionId":"s","cwd":"/x","startedAt":1}"#;
        let raw: RawSession = serde_json::from_str(json).expect("deserialize");
        assert_eq!(raw.status, None);
    }
}
