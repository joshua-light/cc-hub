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

use crate::conversation::classify;
use crate::conversation::{
    parse_timestamp_ms, CurrentTool, EntrySummary, ExplanationStep, StateExplanation, Verdict,
    NO_TEXT_CONTENT,
};
use crate::models::{ConversationMessage, SessionState};
use serde_json::Value;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

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
    let version = session_meta(entries)
        .and_then(|m| m.get("cli_version"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    (None, model, version)
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

fn is_tool_call(entry: &Value) -> bool {
    rec_type(entry) == Some("response_item")
        && matches!(
            payload_type(entry),
            Some("function_call") | Some("custom_tool_call")
        )
}

fn is_tool_call_output(entry: &Value) -> bool {
    rec_type(entry) == Some("response_item")
        && matches!(
            payload_type(entry),
            Some("function_call_output") | Some("custom_tool_call_output")
        )
}

/// The `call_id` that pairs a `function_call`/`custom_tool_call` with its
/// `*_output`. Codex uses `call_id`; some records also carry an `id`.
fn call_id(entry: &Value) -> Option<&str> {
    let p = payload(entry)?;
    p.get("call_id")
        .or_else(|| p.get("id"))
        .and_then(|v| v.as_str())
}

/// The most recent unresolved codex tool call (a `function_call` /
/// `custom_tool_call` without a matching `*_output`) — the tool the agent is
/// currently executing.
pub fn extract_current_tool(entries: &[Value]) -> Option<CurrentTool> {
    let mut unresolved: Vec<(String, String, Option<String>)> = Vec::new();
    let mut results: HashSet<String> = HashSet::new();

    for entry in entries {
        if is_tool_call(entry) {
            let Some(p) = payload(entry) else { continue };
            let id = call_id(entry).unwrap_or("");
            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if id.is_empty() || name.is_empty() {
                continue;
            }
            let hint = format_tool_hint(name, p);
            unresolved.push((id.to_string(), name.to_string(), hint));
        } else if is_tool_call_output(entry) {
            if let Some(id) = call_id(entry) {
                results.insert(id.to_string());
            }
        }
    }

    unresolved
        .into_iter()
        .rev()
        .find(|(id, _, _)| !results.contains(id))
        .map(|(_, name, hint)| CurrentTool { name, hint })
}

/// A one-line hint for a codex tool call. `function_call` arguments are a JSON
/// *string*; `exec_command` carries the shell command under `cmd`. Falls back
/// to the first string value in the parsed arguments/input.
fn format_tool_hint(name: &str, payload: &Value) -> Option<String> {
    let args_val: Option<Value> = payload
        .get("arguments")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .or_else(|| payload.get("input").cloned());
    let args = args_val.as_ref()?;
    let raw = match name {
        "exec_command" | "shell" | "local_shell" => args
            .get("cmd")
            .or_else(|| args.get("command"))
            .and_then(value_as_command),
        _ => args
            .get("cmd")
            .or_else(|| args.get("command"))
            .and_then(value_as_command)
            .or_else(|| {
                args.as_object()
                    .and_then(|o| o.values().find_map(|v| v.as_str().map(str::to_string)))
            }),
    }?;
    let cleaned = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    (!cleaned.is_empty()).then_some(cleaned)
}

/// A command value that may be a plain string or an argv array of strings.
fn value_as_command(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    v.as_array().map(|arr| {
        arr.iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    })
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

/// The text of a codex chat message — user `input_text` blocks or assistant
/// `output_text` blocks joined together.
fn message_text(msg: &Value, max_len: usize) -> Option<String> {
    let content = msg.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(truncate_str(s, max_len));
    }
    let arr = content.as_array()?;
    let mut parts = Vec::new();
    for block in arr {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("input_text") | Some("output_text") | Some("text") => {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    parts.push(t);
                }
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(truncate_str(&parts.join(" "), max_len))
    }
}

/// The text a `user_message` event carries directly under `payload.message`.
fn user_event_text(entry: &Value, max_len: usize) -> Option<String> {
    payload(entry)?
        .get("message")
        .and_then(|v| v.as_str())
        .map(|s| truncate_str(s, max_len))
}

fn is_user_message_record(entry: &Value) -> bool {
    (rec_type(entry) == Some("event_msg") && payload_type(entry) == Some("user_message"))
        || (rec_type(entry) == Some("response_item")
            && payload_type(entry) == Some("message")
            && payload(entry)
                .and_then(|p| p.get("role"))
                .and_then(|r| r.as_str())
                == Some("user"))
}

fn user_record_text(entry: &Value, max_len: usize) -> Option<String> {
    if rec_type(entry) == Some("event_msg") {
        return user_event_text(entry, max_len);
    }
    message_text(payload(entry)?, max_len)
}

/// Codex persists the injected environment bootstrap as a user-shaped record.
/// It is useful to Codex itself, but it is not the user's request and must not
/// become the session's visible name/message in the hub.
fn is_environment_context(text: &str) -> bool {
    text.trim_start().starts_with("<environment_context>")
}

fn displayable_user_text(entry: &Value, max_len: usize) -> Option<String> {
    let text = user_record_text(entry, max_len)?;
    (!is_environment_context(&text)).then_some(text)
}

pub fn extract_last_user_message(entries: &[Value]) -> Option<String> {
    entries
        .iter()
        .rev()
        .filter(|e| is_user_message_record(e))
        .find_map(|e| displayable_user_text(e, 200))
}

pub fn extract_first_user_message(entries: &[Value]) -> Option<String> {
    entries
        .iter()
        .filter(|e| is_user_message_record(e))
        .find_map(|e| displayable_user_text(e, 200))
}

/// Cumulative `(input, output)` token totals — read straight off the most
/// recent `token_count` event's `total_token_usage`, which codex maintains as
/// a running sum for the whole session.
pub fn extract_token_totals(entries: &[Value]) -> (u64, u64) {
    entries
        .iter()
        .rev()
        .find(|e| rec_type(e) == Some("event_msg") && payload_type(e) == Some("token_count"))
        .and_then(|e| payload(e)?.get("info")?.get("total_token_usage").cloned())
        .map(|u| {
            let f = |k: &str| u.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            (f("input_tokens"), f("output_tokens"))
        })
        .unwrap_or((0, 0))
}

pub fn extract_messages(entries: &[Value], count: usize) -> Vec<ConversationMessage> {
    let mut out = Vec::new();
    for entry in entries {
        if rec_type(entry) != Some("response_item") || payload_type(entry) != Some("message") {
            continue;
        }
        let Some(p) = payload(entry) else { continue };
        let role = match p.get("role").and_then(|r| r.as_str()) {
            Some("user") => "user",
            Some("assistant") => "assistant",
            // developer/system messages are not part of the visible dialogue.
            _ => continue,
        };
        let preview = message_text(p, 200).unwrap_or_else(|| NO_TEXT_CONTENT.to_string());
        out.push(ConversationMessage {
            role: role.to_string(),
            content_preview: preview,
            timestamp: record_timestamp(entry).unwrap_or(0),
            model: None,
            stop_reason: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        });
    }
    if out.len() > count {
        out.split_off(out.len() - count)
    } else {
        out
    }
}

/// Streaming counter for codex tool calls (`function_call` /
/// `custom_tool_call` response items). Shared with [`crate::tool_use_count`].
pub fn count_tool_uses_in_reader<R: BufRead>(reader: R) -> usize {
    crate::conversation::count_blocks_in_reader(reader, |val| if is_tool_call(val) { 1 } else { 0 })
}

pub fn count_tool_uses(path: &Path) -> usize {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return 0,
    };
    count_tool_uses_in_reader(BufReader::new(file))
}

pub fn explain_state(entries: &[Value], mtime_age_secs: Option<u64>) -> StateExplanation {
    let final_state = extract_state(entries);
    let last = entries.iter().rev().find(|e| CodexDialect.role_present(e));
    let detail = match last {
        None => "no lifecycle events or messages yet → Idle".to_string(),
        Some(e) => match (rec_type(e), payload_type(e)) {
            (Some("event_msg"), Some(pt)) => {
                format!("last lifecycle event {:?} → {}", pt, final_state)
            }
            (Some("response_item"), _) => {
                let role = payload(e)
                    .and_then(|p| p.get("role"))
                    .and_then(|r| r.as_str())
                    .unwrap_or("?");
                format!("last message role={:?} → {}", role, final_state)
            }
            _ => format!("→ {}", final_state),
        },
    };

    let tail = entries
        .iter()
        .enumerate()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|(idx, entry)| EntrySummary {
            idx,
            kind: match (rec_type(entry), payload_type(entry)) {
                (Some("event_msg"), Some(pt)) => format!("event:{}", pt),
                (Some("response_item"), Some(pt)) => format!("item:{}", pt),
                (Some(t), _) => t.to_string(),
                _ => "?".to_string(),
            },
            timestamp: entry
                .get("timestamp")
                .and_then(|t| t.as_str())
                .map(str::to_string),
            stop_reason: classify::TranscriptDialect::stop_reason(&CodexDialect, entry)
                .map(str::to_string),
            blocks: summarize_blocks(entry),
        })
        .collect();

    StateExplanation {
        final_state: final_state.clone(),
        mtime_age_secs,
        entry_count: entries.len(),
        steps: vec![ExplanationStep {
            name: "codex_last_lifecycle_or_message",
            verdict: Verdict::Decided(final_state),
            details: vec![detail],
        }],
        tail,
    }
}

fn summarize_blocks(entry: &Value) -> Vec<String> {
    match (rec_type(entry), payload_type(entry)) {
        (Some("response_item"), Some("message")) => payload(entry)
            .and_then(|p| p.get("role"))
            .and_then(|r| r.as_str())
            .map(|r| vec![format!("msg:{}", r)])
            .unwrap_or_default(),
        (Some("response_item"), Some("function_call" | "custom_tool_call")) => payload(entry)
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .map(|n| vec![format!("tool:{}", n)])
            .unwrap_or_default(),
        (Some("response_item"), Some("reasoning")) => vec!["reasoning".to_string()],
        _ => Vec::new(),
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    crate::models::first_line_truncated(s.trim(), max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meta(session_id: &str, cwd: &str) -> Value {
        json!({
            "timestamp": "2026-07-14T13:22:22.467Z",
            "type": "session_meta",
            "payload": {"session_id": session_id, "cwd": cwd, "cli_version": "0.144.3"}
        })
    }
    fn event(pt: &str, extra: Value) -> Value {
        let mut p = json!({"type": pt});
        if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                o.insert(k.clone(), v.clone());
            }
        }
        json!({"timestamp": "2026-07-14T13:22:42.639Z", "type": "event_msg", "payload": p})
    }
    fn item(pt: &str, extra: Value) -> Value {
        let mut p = json!({"type": pt});
        if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                o.insert(k.clone(), v.clone());
            }
        }
        json!({"timestamp": "2026-07-14T13:22:50.000Z", "type": "response_item", "payload": p})
    }
    fn turn_context(model: &str) -> Value {
        json!({"timestamp": "2026-07-14T13:22:47.950Z", "type": "turn_context",
            "payload": {"model": model, "turn_id": "t1"}})
    }
    fn user_msg(text: &str) -> Value {
        event("user_message", json!({"message": text}))
    }
    fn assistant_item(text: &str) -> Value {
        item(
            "message",
            json!({"role": "assistant", "content": [{"type": "output_text", "text": text}]}),
        )
    }

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
        assert_eq!(git, None);
        // Most recent turn_context wins.
        assert_eq!(model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(version.as_deref(), Some("0.144.3"));
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
    fn token_totals_from_cumulative() {
        let e = vec![event(
            "token_count",
            json!({"info": {"total_token_usage": {"input_tokens": 900, "output_tokens": 120}}}),
        )];
        assert_eq!(extract_token_totals(&e), (900, 120));
    }

    // --- current tool --------------------------------------------------

    #[test]
    fn current_tool_unresolved_exec_command() {
        let e = vec![item(
            "function_call",
            json!({"name": "exec_command", "call_id": "c1",
                "arguments": "{\"cmd\":\"cargo  build\",\"workdir\":\"/x\"}"}),
        )];
        let t = extract_current_tool(&e).unwrap();
        assert_eq!(t.name, "exec_command");
        assert_eq!(t.hint.as_deref(), Some("cargo build"));
    }

    #[test]
    fn current_tool_none_when_output_present() {
        let e = vec![
            item(
                "function_call",
                json!({"name": "exec_command", "call_id": "c1",
                    "arguments": "{\"cmd\":\"ls\"}"}),
            ),
            item(
                "function_call_output",
                json!({"call_id": "c1", "output": "ok"}),
            ),
        ];
        assert_eq!(extract_current_tool(&e), None);
    }

    #[test]
    fn current_tool_command_array_hint() {
        let e = vec![item(
            "function_call",
            json!({"name": "shell", "call_id": "c1",
                "arguments": "{\"command\":[\"bash\",\"-lc\",\"ls -la\"]}"}),
        )];
        assert_eq!(
            extract_current_tool(&e).unwrap().hint.as_deref(),
            Some("bash -lc ls -la")
        );
    }

    // --- user messages -------------------------------------------------

    #[test]
    fn last_user_message_prefers_event() {
        let e = vec![
            user_msg("first"),
            assistant_item("reply"),
            user_msg("second"),
        ];
        assert_eq!(extract_last_user_message(&e).as_deref(), Some("second"));
        assert_eq!(extract_first_user_message(&e).as_deref(), Some("first"));
    }

    #[test]
    fn user_message_from_response_item() {
        let e = vec![item(
            "message",
            json!({"role": "user", "content": [{"type": "input_text", "text": "hi there"}]}),
        )];
        assert_eq!(extract_last_user_message(&e).as_deref(), Some("hi there"));
    }

    #[test]
    fn environment_context_is_not_a_visible_user_message() {
        let e = vec![
            user_msg("initial request"),
            assistant_item("working"),
            user_msg("<environment_context>\nrepo metadata\n</environment_context>"),
        ];
        assert_eq!(
            extract_last_user_message(&e).as_deref(),
            Some("initial request")
        );
        assert_eq!(
            extract_first_user_message(&e).as_deref(),
            Some("initial request")
        );
    }

    #[test]
    fn environment_context_only_does_not_name_a_session() {
        let e = vec![user_msg(
            "<environment_context>\nrepo metadata\n</environment_context>",
        )];
        assert_eq!(extract_last_user_message(&e), None);
        assert_eq!(extract_first_user_message(&e), None);
    }

    // --- messages / thinking / tool counts -----------------------------

    #[test]
    fn extract_messages_maps_user_and_assistant() {
        let e = vec![
            item(
                "message",
                json!({"role": "user", "content": [{"type": "input_text", "text": "hello"}]}),
            ),
            item("reasoning", json!({"summary": "thinking"})),
            assistant_item("hi back"),
        ];
        let msgs = extract_messages(&e, 10);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content_preview, "hello");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].content_preview, "hi back");
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

    #[test]
    fn count_tool_uses_counts_function_and_custom_calls() {
        let data = format!(
            "{}\n{}\n{}\n{}\n",
            item(
                "function_call",
                json!({"name": "exec_command", "call_id": "a"})
            ),
            item("message", json!({"role": "assistant", "content": []})),
            item(
                "custom_tool_call",
                json!({"name": "apply_patch", "call_id": "b"})
            ),
            item("function_call_output", json!({"call_id": "a"})),
        );
        assert_eq!(count_tool_uses_in_reader(data.as_bytes()), 2);
    }
}
