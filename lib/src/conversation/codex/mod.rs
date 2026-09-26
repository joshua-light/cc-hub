//! Codex (`~/.codex/sessions/**/rollout-*.jsonl`) transcript parsing.
//!
//! Codex's rollout format is structurally different from Claude/Pi: every
//! record is `{"timestamp", "type", "payload"}`, and the turn lifecycle lives
//! in `event_msg` records (`task_started` / `task_complete` / `turn_aborted`),
//! not in per-message stop reasons. So the [`CodexDialect`] answers the shared
//! [`classify`] state machine's format questions by mapping those lifecycle
//! events onto assistant "stop reasons": a `task_started` reads as an in-flight
//! assistant turn (Processing), a `task_complete`/`turn_aborted` as end-of-turn
//! (WaitingForInput). The *meaning* still lives once in
//! [`crate::conversation::classify`], shared with every other backend.
//!
//! - [`tools`] — tool-call detection, the current tool, and tool-use counts.
//! - [`messages`] — user/assistant message text and token totals.

mod messages;
#[cfg(test)]
mod test_util;
mod tools;

use crate::conversation::classify;
use crate::conversation::parse_timestamp_ms;
use crate::models::SessionState;
use serde_json::Value;
use std::path::Path;

pub use messages::{
    extract_first_user_message, extract_last_user_message, extract_messages, extract_token_totals,
};
pub use tools::{count_tool_uses_in_reader, extract_current_tool};

// --- record accessors ---------------------------------------------------

/// Top-level record type (`session_meta` | `turn_context` | `event_msg` |
/// `response_item` | …).
fn rec_type(entry: &Value) -> Option<&str> {
    entry.get("type").and_then(|t| t.as_str())
}

fn payload(entry: &Value) -> Option<&Value> {
    entry.get("payload")
}

/// The `payload.type` discriminator inside an `event_msg` / `response_item`.
fn payload_type(entry: &Value) -> Option<&str> {
    payload(entry)?.get("type").and_then(|t| t.as_str())
}

/// Timestamp of a record in epoch-ms, from the top-level ISO `timestamp`.
fn record_timestamp(entry: &Value) -> Option<u64> {
    entry.get("timestamp").and_then(parse_timestamp_ms)
}

/// The `session_meta` payload (session id, cwd, cli_version, …). Present as the
/// first record of every well-formed rollout.
fn session_meta(entries: &[Value]) -> Option<&Value> {
    entries
        .iter()
        .find(|e| rec_type(e) == Some("session_meta"))
        .and_then(payload)
}

// --- the shared state machine's format adapter --------------------------

/// The Codex rollout dialect. Only the turn-lifecycle `event_msg` records and
/// `response_item` chat messages bear a conversational role for the state
/// machine; reasoning items, function calls, token counts, and world-state
/// snapshots are skipped (role `None`).
pub(crate) struct CodexDialect;

impl classify::TranscriptDialect for CodexDialect {
    const NAME: &'static str = "codex";

    fn role(&self, entry: &Value) -> Option<classify::Role> {
        match rec_type(entry)? {
            "event_msg" => match payload_type(entry)? {
                // A user turn begins; the agent is about to work.
                "user_message" => Some(classify::Role::User),
                // Turn lifecycle: treat as an assistant entry whose stop
                // reason [`Self::stop_reason`] then maps to running vs. done.
                "task_started" | "task_complete" | "turn_aborted" => {
                    Some(classify::Role::Assistant)
                }
                _ => None,
            },
            "response_item" => {
                if payload_type(entry)? != "message" {
                    return None;
                }
                // developer messages are injected permission/system context —
                // not a conversational turn, so the state machine skips them.
                match payload(entry)?.get("role").and_then(|r| r.as_str())? {
                    "user" => Some(classify::Role::User),
                    "assistant" => Some(classify::Role::Assistant),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn stop_reason<'a>(&self, entry: &'a Value) -> Option<&'a str> {
        // Only the lifecycle events carry an end-of-turn signal. `task_started`
        // and streamed assistant messages return None → the classifier reads
        // them as Processing without consulting `map_stop`.
        if rec_type(entry) == Some("event_msg") {
            return match payload_type(entry) {
                Some("task_complete") => Some("complete"),
                Some("turn_aborted") => Some("aborted"),
                _ => None,
            };
        }
        None
    }

    fn map_stop(&self, stop: &str) -> classify::StopMapping {
        match stop {
            // Codex ends a turn on completion or abort — either way the agent
            // has stopped and waits for the user's next prompt.
            "complete" | "aborted" => classify::StopMapping::EndOfTurn,
            _ => classify::StopMapping::Unknown,
        }
    }

    fn is_interrupt_marker(&self, _entry: &Value) -> bool {
        // Codex records an explicit `turn_aborted` event instead of a synthetic
        // user-text marker, so there is nothing to detect here.
        false
    }

    fn blocking_tool(&self, _entry: &Value) -> classify::BlockingTool {
        // Codex approvals surface as their own flow, not an in-transcript tool
        // call the way Claude's AskUserQuestion does. No blocking tool today.
        classify::BlockingTool::None
    }
}

pub fn extract_state(entries: &[Value]) -> SessionState {
    classify::classify(&CodexDialect, entries, None)
}

/// Expand the tail window until it holds a Codex role-bearing record (a
/// lifecycle event or a chat message) — the codex analogue of
/// [`crate::conversation::read_jsonl_tail_for_state`], which keys on a Claude
/// `type=="assistant"` line that codex transcripts never contain.
pub fn read_jsonl_tail_for_state(path: &Path) -> Vec<Value> {
    const INITIAL: u64 = 64 * 1024;
    const MAX: u64 = 4 * 1024 * 1024;

    let total_len = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return Vec::new(),
    };

    let mut window = INITIAL;
    loop {
        let entries = crate::conversation::read_jsonl_tail(path, window);
        let has_role = entries.iter().any(|e| CodexDialect.role_present(e));
        if has_role || window >= total_len || window >= MAX {
            return entries;
        }
        window = window.saturating_mul(2);
    }
}

impl CodexDialect {
    /// Whether `entry` carries a conversational role (used to size the tail
    /// window). Mirrors [`classify::TranscriptDialect::role`] returning `Some`.
    fn role_present(&self, entry: &Value) -> bool {
        classify::TranscriptDialect::role(self, entry).is_some()
    }
}

// --- SessionInfo field extractors --------------------------------------

/// Codex's session id: `session_meta.payload.session_id`.
pub fn extract_session_id(entries: &[Value]) -> Option<String> {
    let meta = session_meta(entries)?;
    meta.get("session_id")
        .or_else(|| meta.get("id"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// The working directory the session runs in: `session_meta.payload.cwd`.
pub fn extract_cwd(entries: &[Value]) -> Option<String> {
    session_meta(entries)?
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// When the session started, from the `session_meta` record timestamp.
pub fn extract_started_at(entries: &[Value]) -> u64 {
    entries
        .iter()
        .find(|e| rec_type(e) == Some("session_meta"))
        .and_then(record_timestamp)
        .or_else(|| {
            session_meta(entries)
                .and_then(|m| m.get("timestamp"))
                .and_then(parse_timestamp_ms)
        })
        .unwrap_or(0)
}

/// `(git_branch, model, version)` — codex records no git branch, so that is
/// always `None`. The model is the most recent `turn_context.model`; the
/// version is `session_meta.cli_version`.
pub fn extract_metadata(entries: &[Value]) -> (Option<String>, Option<String>, Option<String>) {
    let model = entries
        .iter()
        .rev()
        .find(|e| rec_type(e) == Some("turn_context"))
        .and_then(|e| payload(e)?.get("model").and_then(|v| v.as_str()))
        .map(str::to_string);
    let meta = session_meta(entries);
    let version = meta
        .and_then(|m| m.get("cli_version"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    // Codex records the repo state once, in `session_meta.git`, rather than
    // per-entry like Claude — so the branch only ever comes from the head.
    let git_branch = meta
        .and_then(|m| m.get("git")?.get("branch")?.as_str())
        .filter(|b| !b.is_empty())
        .map(str::to_string);
    (git_branch, model, version)
}

pub fn extract_last_activity(entries: &[Value]) -> Option<u64> {
    entries.iter().filter_map(record_timestamp).max()
}

/// Live context-window utilisation: the `input_tokens` of the most recent
/// `token_count` event (the size of the prompt re-sent next turn, already
/// inclusive of cached input). Falls back to the cumulative total, then None.
pub fn extract_context_tokens(entries: &[Value]) -> Option<u64> {
    let info = entries
        .iter()
        .rev()
        .find(|e| rec_type(e) == Some("event_msg") && payload_type(e) == Some("token_count"))
        .and_then(|e| payload(e)?.get("info"))?;
    let last_input = info
        .get("last_token_usage")
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_u64());
    let total = info
        .get("total_token_usage")
        .and_then(|u| u.get("total_tokens"))
        .and_then(|v| v.as_u64());
    match last_input.or(total) {
        Some(n) if n > 0 => Some(n),
        _ => None,
    }
}

pub fn is_currently_thinking(entries: &[Value]) -> bool {
    // Reasoning-in-progress: the most recent role/reasoning record is a
    // `reasoning` item with no assistant message or lifecycle event after it.
    for entry in entries.iter().rev() {
        match (rec_type(entry), payload_type(entry)) {
            (Some("response_item"), Some("reasoning")) => return true,
            (Some("response_item"), Some("message")) => return false,
            (Some("event_msg"), Some("agent_message")) => return false,
            (Some("event_msg"), Some("task_complete"))
            | (Some("event_msg"), Some("turn_aborted")) => return false,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::test_util::*;
    use super::*;
    use serde_json::json;

    // --- extract_state -------------------------------------------------

    #[test]
    fn empty_is_idle() {
        assert_eq!(extract_state(&[]), SessionState::Idle);
    }

    #[test]
    fn only_session_meta_is_idle() {
        assert_eq!(extract_state(&[meta("s1", "/tmp")]), SessionState::Idle);
    }

    #[test]
    fn user_message_is_processing() {
        let e = vec![meta("s1", "/tmp"), user_msg("do it")];
        assert_eq!(extract_state(&e), SessionState::Processing);
    }

    #[test]
    fn task_started_is_processing() {
        let e = vec![
            user_msg("do it"),
            event("task_started", json!({"turn_id": "t1"})),
        ];
        assert_eq!(extract_state(&e), SessionState::Processing);
    }

    #[test]
    fn task_complete_is_waiting() {
        let e = vec![
            user_msg("do it"),
            event("task_started", json!({})),
            assistant_item("all done"),
            event("task_complete", json!({"turn_id": "t1"})),
        ];
        assert_eq!(extract_state(&e), SessionState::WaitingForInput);
    }

    #[test]
    fn turn_aborted_is_waiting() {
        let e = vec![
            event("task_started", json!({})),
            event("turn_aborted", json!({})),
        ];
        assert_eq!(extract_state(&e), SessionState::WaitingForInput);
    }

    #[test]
    fn running_tool_call_is_processing() {
        // task_started, then a function_call still in flight → Processing (the
        // last role-bearing record is task_started; the function_call itself
        // has no role but does not end the turn).
        let e = vec![
            user_msg("go"),
            event("task_started", json!({})),
            item(
                "function_call",
                json!({"name": "exec_command", "call_id": "c1",
                    "arguments": "{\"cmd\":\"ls\"}"}),
            ),
        ];
        assert_eq!(extract_state(&e), SessionState::Processing);
    }

    #[test]
    fn developer_message_is_skipped() {
        // A trailing developer message must not be read as an assistant turn.
        let e = vec![
            event("task_complete", json!({})),
            item(
                "message",
                json!({"role": "developer", "content": [{"type": "input_text", "text": "sandbox"}]}),
            ),
        ];
        assert_eq!(extract_state(&e), SessionState::WaitingForInput);
    }

    // --- metadata / ids ------------------------------------------------

    #[test]
    fn extract_ids_and_model() {
        let e = vec![
            meta("019f-abc", "/home/u/proj"),
            turn_context("gpt-5.4-mini"),
            turn_context("gpt-5.6-luna"),
        ];
        assert_eq!(extract_session_id(&e).as_deref(), Some("019f-abc"));
        assert_eq!(extract_cwd(&e).as_deref(), Some("/home/u/proj"));
        let (git, model, version) = extract_metadata(&e);
        // No `git` block in session_meta (non-repo cwd) → no branch.
        assert_eq!(git, None);
        // Most recent turn_context wins.
        assert_eq!(model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(version.as_deref(), Some("0.144.3"));
    }

    #[test]
    fn extract_branch_from_session_meta_git_block() {
        let e = vec![json!({
            "timestamp": "2026-07-26T15:51:28.614Z",
            "type": "session_meta",
            "payload": {
                "session_id": "019f-abc",
                "cwd": "/home/u/proj",
                "cli_version": "0.145.0",
                "git": {
                    "commit_hash": "744e6885fb70685fd74a67ca34145960f0f6bcdd",
                    "branch": "experiment/jobified-ecs",
                    "repository_url": "ssh://git@example.com/proj.git"
                }
            }
        })];
        let (git, _, _) = extract_metadata(&e);
        assert_eq!(git.as_deref(), Some("experiment/jobified-ecs"));
    }

    #[test]
    fn empty_branch_is_treated_as_absent() {
        // A detached HEAD records the git block with an empty branch; that must
        // not surface as a blank branch chip.
        let e = vec![json!({
            "type": "session_meta",
            "payload": {"session_id": "s", "cwd": "/p", "git": {"branch": ""}}
        })];
        assert_eq!(extract_metadata(&e).0, None);
    }

    #[test]
    fn context_tokens_from_last_token_count() {
        let e = vec![event(
            "token_count",
            json!({"info": {
                "last_token_usage": {"input_tokens": 15851, "output_tokens": 308},
                "total_token_usage": {"total_tokens": 16159},
                "model_context_window": 258400
            }}),
        )];
        assert_eq!(extract_context_tokens(&e), Some(15851));
    }

    #[test]
    fn context_tokens_falls_back_to_total() {
        let e = vec![event(
            "token_count",
            json!({"info": {"total_token_usage": {"total_tokens": 4242}}}),
        )];
        assert_eq!(extract_context_tokens(&e), Some(4242));
    }

    #[test]
    fn thinking_true_when_reasoning_trails() {
        let e = vec![
            event("task_started", json!({})),
            item("reasoning", json!({"summary": "…"})),
        ];
        assert!(is_currently_thinking(&e));
    }

    #[test]
    fn thinking_false_when_message_follows_reasoning() {
        let e = vec![item("reasoning", json!({})), assistant_item("done")];
        assert!(!is_currently_thinking(&e));
    }
}
