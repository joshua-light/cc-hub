//! Pi (`~/.pi/agent/sessions/<project>/*.jsonl`) transcript parsing.
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
        let has_assistant = entries.iter().any(|e| {
            e.get("type").and_then(|t| t.as_str()) == Some("message")
                && e.get("message")
                    .and_then(|m| m.get("role"))
                    .and_then(|r| r.as_str())
                    == Some("assistant")
        });
        if has_assistant || window >= total_len || window >= MAX {
            return entries;
        }
        window = window.saturating_mul(2);
    }
}

fn message_role(entry: &Value) -> Option<&str> {
    (entry.get("type").and_then(|t| t.as_str()) == Some("message"))
        .then(|| entry.get("message")?.get("role")?.as_str())
        .flatten()
}

fn assistant_stop_reason(entry: &Value) -> Option<&str> {
    entry.get("message")?.get("stopReason")?.as_str()
}

fn message_timestamp(entry: &Value) -> Option<u64> {
    entry
        .get("timestamp")
        .and_then(parse_timestamp_ms)
        .or_else(|| {
            entry
                .get("message")
                .and_then(|m| m.get("timestamp"))
                .and_then(parse_timestamp_ms)
        })
}

fn content_text(content: &Value, max_len: usize) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(truncate_str(text, max_len));
    }
    let arr = content.as_array()?;
    for block in arr {
        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                return Some(truncate_str(text, max_len));
            }
        }
    }
    None
}

/// The Pi JSONL dialect: format adapters over this module's entry helpers.
/// The state *semantics* live in [`classify::classify`], shared with the
/// Claude backend. Pi has no interrupt marker and no blocking tools today,
/// so those adapters are constant — if Pi grows either, this is the one
/// place to teach cc-hub about it.
pub(crate) struct PiDialect;

impl classify::TranscriptDialect for PiDialect {
    const NAME: &'static str = "pi";

    fn role(&self, entry: &Value) -> Option<classify::Role> {
        match message_role(entry) {
            Some("user") => Some(classify::Role::User),
            Some("assistant") => Some(classify::Role::Assistant),
            _ => None,
        }
    }

    fn stop_reason<'a>(&self, entry: &'a Value) -> Option<&'a str> {
        assistant_stop_reason(entry)
    }

    fn map_stop(&self, stop: &str) -> classify::StopMapping {
        match stop {
            "toolUse" => classify::StopMapping::ToolUse,
            // Pi ends a turn on error/abort/length too — the agent stopped
            // and waits for the user either way.
            "stop" | "error" | "aborted" | "length" => classify::StopMapping::EndOfTurn,
            _ => classify::StopMapping::Unknown,
        }
    }

    fn is_interrupt_marker(&self, _entry: &Value) -> bool {
        false
    }

    fn blocking_tool(&self, _entry: &Value) -> classify::BlockingTool {
        classify::BlockingTool::None
    }
}

pub fn extract_state(entries: &[Value]) -> SessionState {
    classify::classify(&PiDialect, entries, None)
}

pub fn is_currently_thinking(entries: &[Value]) -> bool {
    entries
        .iter()
        .rev()
        .find(|e| message_role(e) == Some("assistant"))
        .and_then(|e| {
            let arr = e.get("message")?.get("content")?.as_array()?;
            let last = arr.last()?;
            Some(last.get("type").and_then(|t| t.as_str()) == Some("thinking"))
        })
        .unwrap_or(false)
}

pub fn extract_context_tokens(entries: &[Value]) -> Option<u64> {
    entries
        .iter()
        .rev()
        .find(|e| message_role(e) == Some("assistant"))
        .and_then(|e| {
            let usage = e.get("message")?.get("usage")?;
            let f = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            let total = f("input") + f("cacheRead") + f("cacheWrite");
            if total == 0 {
                None
            } else {
                Some(total)
            }
        })
}

pub fn extract_metadata(entries: &[Value]) -> (Option<String>, Option<String>, Option<String>) {
    let model_change = entries.iter().rev().find_map(|e| {
        (e.get("type").and_then(|t| t.as_str()) == Some("model_change")).then(|| {
            let provider = e.get("provider").and_then(|v| v.as_str());
            let model_id = e.get("modelId").and_then(|v| v.as_str());
            match (provider, model_id) {
                (Some(p), Some(m)) => Some(format!("{}/{}", p, m)),
                (_, Some(m)) => Some(m.to_string()),
                _ => None,
            }
        })
    });

    let model = model_change.flatten().or_else(|| {
        entries
            .iter()
            .rev()
            .find_map(|e| {
                (message_role(e) == Some("assistant")).then(|| {
                    let msg = e.get("message")?;
                    let provider = msg.get("provider").and_then(|v| v.as_str());
                    let model = msg.get("model").and_then(|v| v.as_str());
                    match (provider, model) {
                        (Some(p), Some(m)) => Some(format!("{}/{}", p, m)),
                        (_, Some(m)) => Some(m.to_string()),
                        _ => None,
                    }
                })
            })
            .flatten()
    });

    (None, model, None)
}

pub fn extract_last_activity(entries: &[Value]) -> Option<u64> {
    entries.iter().filter_map(message_timestamp).max()
}

fn truncate_str(s: &str, max: usize) -> String {
    crate::models::first_line_truncated(s.trim(), max)
}

#[cfg(test)]
mod tests {
    use super::test_util::*;
    use super::*;
    use serde_json::json;

    // --- extract_state ---

    #[test]
    fn extract_state_empty_is_idle() {
        assert_eq!(extract_state(&[]), SessionState::Idle);
    }

    #[test]
    fn extract_state_only_non_message_entries_is_idle() {
        // model_change / toolResult entries are not user|assistant roles.
        let entries = vec![
            json!({"type": "model_change", "provider": "anthropic", "modelId": "x"}),
            tool_result("t1"),
        ];
        assert_eq!(extract_state(&entries), SessionState::Idle);
    }

    #[test]
    fn extract_state_last_user_is_processing() {
        let entries = vec![
            assistant("stop", json!([{"type": "text", "text": "done"}])),
            user(json!("another prompt")),
        ];
        assert_eq!(extract_state(&entries), SessionState::Processing);
    }

    #[test]
    fn extract_state_assistant_tool_use_is_processing() {
        let entries = vec![assistant(
            "toolUse",
            json!([tool_call("t1", "bash", json!({"command": "ls"}))]),
        )];
        assert_eq!(extract_state(&entries), SessionState::Processing);
    }

    #[test]
    fn extract_state_assistant_stop_is_waiting() {
        let entries = vec![assistant("stop", json!([{"type": "text", "text": "hi"}]))];
        assert_eq!(extract_state(&entries), SessionState::WaitingForInput);
    }

    #[test]
    fn extract_state_assistant_error_aborted_length_are_waiting() {
        for reason in ["error", "aborted", "length"] {
            let entries = vec![assistant(reason, json!([]))];
            assert_eq!(
                extract_state(&entries),
                SessionState::WaitingForInput,
                "stopReason={reason} should be WaitingForInput"
            );
        }
    }

    #[test]
    fn extract_state_assistant_unknown_stop_reason_is_processing() {
        // Missing/unrecognised stopReason falls through to Processing.
        let entries = vec![assistant("", json!([]))];
        assert_eq!(extract_state(&entries), SessionState::Processing);
        let entries = vec![json!({"type": "message", "message": {"role": "assistant"}})];
        assert_eq!(extract_state(&entries), SessionState::Processing);
    }

    #[test]
    fn extract_state_skips_trailing_tool_result_to_find_assistant() {
        // toolResult is not a user|assistant role, so state is judged by the
        // preceding assistant entry.
        let entries = vec![
            assistant("stop", json!([{"type": "text", "text": "answer"}])),
            tool_result("t1"),
        ];
        assert_eq!(extract_state(&entries), SessionState::WaitingForInput);
    }

    #[test]
    fn extract_state_ignores_type_assistant_without_message_wrapper() {
        // Claude-shaped entry (type=assistant directly) is NOT a Pi message —
        // message_role returns None, so it's invisible to the Pi parser.
        let entries = vec![json!({
            "type": "assistant",
            "message": {"role": "assistant", "stopReason": "stop", "content": []}
        })];
        assert_eq!(extract_state(&entries), SessionState::Idle);
    }

    // --- extract_context_tokens ---

    #[test]
    fn extract_context_tokens_sums_input_and_cache() {
        let entries = vec![json!({
            "type": "message",
            "message": {"role": "assistant", "usage": {
                "input": 1000, "cacheRead": 50000, "cacheWrite": 4000, "output": 200
            }}
        })];
        assert_eq!(extract_context_tokens(&entries), Some(55000));
    }

    #[test]
    fn extract_context_tokens_none_without_assistant() {
        let entries = vec![user(json!("hi"))];
        assert_eq!(extract_context_tokens(&entries), None);
    }

    #[test]
    fn extract_context_tokens_none_when_total_zero() {
        let entries = vec![json!({
            "type": "message",
            "message": {"role": "assistant", "usage": {"output": 200}}
        })];
        assert_eq!(extract_context_tokens(&entries), None);
    }

    #[test]
    fn extract_context_tokens_uses_most_recent_assistant() {
        let entries = vec![
            json!({"type": "message", "message": {"role": "assistant",
                "usage": {"input": 5}}}),
            json!({"type": "message", "message": {"role": "assistant",
                "usage": {"input": 7, "cacheRead": 3}}}),
        ];
        assert_eq!(extract_context_tokens(&entries), Some(10));
    }

    // --- extract_metadata ---

    #[test]
    fn extract_metadata_model_change_provider_and_id() {
        let entries = vec![json!({
            "type": "model_change", "provider": "anthropic", "modelId": "claude-opus-4"
        })];
        let (git, model, version) = extract_metadata(&entries);
        assert_eq!(git, None);
        assert_eq!(model.as_deref(), Some("anthropic/claude-opus-4"));
        assert_eq!(version, None);
    }

    #[test]
    fn extract_metadata_model_change_id_only() {
        let entries = vec![json!({"type": "model_change", "modelId": "gpt-5"})];
        assert_eq!(extract_metadata(&entries).1.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn extract_metadata_falls_back_to_assistant_message() {
        // No model_change → use the most recent assistant message provider/model.
        let entries = vec![json!({
            "type": "message",
            "message": {"role": "assistant", "provider": "openai", "model": "o3"}
        })];
        assert_eq!(extract_metadata(&entries).1.as_deref(), Some("openai/o3"));
    }

    #[test]
    fn extract_metadata_model_change_takes_precedence_over_assistant() {
        let entries = vec![
            json!({"type": "message", "message": {"role": "assistant",
                "provider": "openai", "model": "o3"}}),
            json!({"type": "model_change", "provider": "anthropic",
                "modelId": "claude-opus-4"}),
        ];
        assert_eq!(
            extract_metadata(&entries).1.as_deref(),
            Some("anthropic/claude-opus-4")
        );
    }

    #[test]
    fn extract_metadata_none_when_no_model_info() {
        let entries = vec![user(json!("hi"))];
        assert_eq!(extract_metadata(&entries), (None, None, None));
    }

    // --- extract_last_activity ---

    #[test]
    fn extract_last_activity_top_level_timestamp() {
        let entries = vec![
            json!({"type": "message", "timestamp": "2026-04-15T18:14:00.000Z",
                "message": {"role": "user", "content": "a"}}),
            json!({"type": "message", "timestamp": "2026-04-15T18:14:30.201Z",
                "message": {"role": "assistant", "stopReason": "stop", "content": []}}),
        ];
        let ms = extract_last_activity(&entries).unwrap();
        assert_eq!(ms % 1000, 201);
    }

    #[test]
    fn extract_last_activity_nested_message_timestamp() {
        // Timestamp lives under message, not top-level.
        let entries = vec![json!({
            "type": "message",
            "message": {"role": "user", "content": "a",
                "timestamp": "2026-04-15T18:14:30.201Z"}
        })];
        let ms = extract_last_activity(&entries).unwrap();
        assert_eq!(ms % 1000, 201);
    }

    #[test]
    fn extract_last_activity_returns_max_not_last() {
        // Out-of-order timestamps: extract_last_activity uses .max().
        let entries = vec![
            json!({"type": "message", "timestamp": 5000u64,
                "message": {"role": "user"}}),
            json!({"type": "message", "timestamp": 1000u64,
                "message": {"role": "assistant", "content": []}}),
        ];
        assert_eq!(extract_last_activity(&entries), Some(5000));
    }

    #[test]
    fn extract_last_activity_none_without_timestamps() {
        let entries = vec![user(json!("hi"))];
        assert_eq!(extract_last_activity(&entries), None);
    }

    // --- read_jsonl_tail_for_state ---

    #[test]
    fn read_jsonl_tail_for_state_missing_file_is_empty() {
        let path = Path::new("/nonexistent/cc_hub_pi_does_not_exist.jsonl");
        assert!(read_jsonl_tail_for_state(path).is_empty());
    }

    #[test]
    fn read_jsonl_tail_for_state_reads_pi_entries() {
        let f = write_jsonl(&[
            r#"{"type":"message","message":{"role":"user","content":"hello"}}"#,
            r#"{"type":"message","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"hi"}]}}"#,
        ]);
        let entries = read_jsonl_tail_for_state(f.path());
        assert_eq!(entries.len(), 2);
        assert_eq!(extract_state(&entries), SessionState::WaitingForInput);
    }

    #[test]
    fn read_jsonl_tail_for_state_skips_garbage_trailing_line() {
        // The last line is truncated/garbage JSON; parse_jsonl_values drops it
        // while keeping the well-formed entries before it.
        let f = write_jsonl(&[
            r#"{"type":"message","message":{"role":"user","content":"hello"}}"#,
            r#"{"type":"message","message":{"role":"assistant","stopReason":"stop","content":[]}}"#,
            r#"{"type":"message","message":{"role":"assist"#, // truncated, invalid JSON
        ]);
        let entries = read_jsonl_tail_for_state(f.path());
        assert_eq!(entries.len(), 2, "garbage trailing line must be discarded");
        assert_eq!(
            message_role(&entries[1]),
            Some("assistant"),
            "the two valid Pi entries survive"
        );
    }

    // --- is_currently_thinking ---

    #[test]
    fn is_currently_thinking_true_when_last_block_thinking() {
        let entries = vec![assistant("toolUse", json!([{"type": "thinking"}]))];
        assert!(is_currently_thinking(&entries));
    }

    #[test]
    fn is_currently_thinking_false_when_tool_call_follows() {
        let entries = vec![
            assistant("toolUse", json!([{"type": "thinking"}])),
            assistant(
                "toolUse",
                json!([tool_call("t1", "bash", json!({"command": "ls"}))]),
            ),
        ];
        assert!(!is_currently_thinking(&entries));
    }
}
