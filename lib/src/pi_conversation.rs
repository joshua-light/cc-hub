use crate::conversation::{
    parse_timestamp_ms, CurrentTool, EntrySummary, ExplanationStep, StateExplanation, Verdict,
    NO_CONTENT, NO_TEXT_CONTENT, THINKING_MARKER, TOOL_MARKER_PREFIX,
};
use crate::models::{ConversationMessage, SessionState};
use serde_json::Value;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Count assistant `toolCall` blocks across an entire Pi JSONL transcript.
/// Streams line-by-line; returns 0 on missing/unreadable file.
pub fn count_tool_uses(path: &Path) -> usize {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return 0,
    };
    count_tool_uses_in_reader(BufReader::new(file))
}

/// Streaming counter for `toolCall` blocks reading from any `BufRead`.
/// Shared with [`crate::tool_use_count`] for incremental updates.
pub fn count_tool_uses_in_reader<R: BufRead>(reader: R) -> usize {
    crate::conversation::count_blocks_in_reader(reader, |val| {
        // Pi wraps assistant entries inside `type=message` with
        // `message.role=assistant`; Claude uses `type=assistant` directly.
        if val.get("type").and_then(|t| t.as_str()) != Some("message") {
            return 0;
        }
        if val
            .get("message")
            .and_then(|m| m.get("role"))
            .and_then(|r| r.as_str())
            != Some("assistant")
        {
            return 0;
        }
        crate::conversation::count_blocks_of_type(val, "toolCall")
    })
}

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

pub fn extract_state(entries: &[Value]) -> SessionState {
    let last = entries
        .iter()
        .rev()
        .find(|e| matches!(message_role(e), Some("user") | Some("assistant")));
    let Some(last) = last else {
        return SessionState::Idle;
    };
    match message_role(last) {
        Some("user") => SessionState::Processing,
        Some("assistant") => match assistant_stop_reason(last).unwrap_or("") {
            "toolUse" => SessionState::Processing,
            "stop" | "error" | "aborted" | "length" => SessionState::WaitingForInput,
            _ => SessionState::Processing,
        },
        _ => SessionState::Idle,
    }
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

pub fn extract_current_tool(entries: &[Value]) -> Option<CurrentTool> {
    let mut unresolved: Vec<(String, String, Option<String>)> = Vec::new();
    let mut results: HashSet<String> = HashSet::new();

    for entry in entries {
        match message_role(entry) {
            Some("assistant") => {
                let Some(arr) = entry
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                else {
                    continue;
                };
                for block in arr {
                    if block.get("type").and_then(|t| t.as_str()) != Some("toolCall") {
                        continue;
                    }
                    let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    if id.is_empty() || name.is_empty() {
                        continue;
                    }
                    let hint = format_tool_hint(name, block.get("arguments"));
                    unresolved.push((id.to_string(), name.to_string(), hint));
                }
            }
            Some("toolResult") => {
                if let Some(id) = entry
                    .get("message")
                    .and_then(|m| m.get("toolCallId"))
                    .and_then(|v| v.as_str())
                {
                    results.insert(id.to_string());
                }
            }
            _ => {}
        }
    }

    unresolved
        .into_iter()
        .rev()
        .find(|(id, _, _)| !results.contains(id))
        .map(|(_, name, hint)| CurrentTool { name, hint })
}

fn format_tool_hint(name: &str, args: Option<&Value>) -> Option<String> {
    let args = args?;
    let raw = match name {
        "bash" => args
            .get("command")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        "read" | "write" | "edit" | "grep" | "find" | "ls" => args
            .get("path")
            .and_then(|v| v.as_str())
            .or_else(|| args.get("pattern").and_then(|v| v.as_str()))
            .map(str::to_string),
        _ => args
            .as_object()
            .and_then(|o| o.values().find_map(|v| v.as_str().map(str::to_string))),
    }?;
    let cleaned = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    (!cleaned.is_empty()).then_some(cleaned)
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

pub fn extract_last_user_message(entries: &[Value]) -> Option<String> {
    entries
        .iter()
        .rev()
        .find(|e| message_role(e) == Some("user"))
        .and_then(|e| extract_user_text(e, 200))
}

pub fn extract_first_user_message(entries: &[Value]) -> Option<String> {
    entries
        .iter()
        .find(|e| message_role(e) == Some("user"))
        .and_then(|e| extract_user_text(e, 200))
}

fn extract_user_text(entry: &Value, max_len: usize) -> Option<String> {
    let content = entry.get("message")?.get("content")?;
    content_text(content, max_len)
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

pub fn extract_messages(entries: &[Value], count: usize) -> Vec<ConversationMessage> {
    let mut out = Vec::new();
    for entry in entries {
        let Some(role) = message_role(entry) else {
            continue;
        };
        let preview = extract_text_content(entry);
        let timestamp = message_timestamp(entry).unwrap_or(0);
        let (model, stop_reason, usage) = if role == "assistant" {
            let msg = entry.get("message");
            (
                msg.and_then(|m| m.get("model"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                msg.and_then(|m| m.get("stopReason"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                msg.and_then(|m| m.get("usage")),
            )
        } else {
            (None, None, None)
        };
        out.push(ConversationMessage {
            role: role.to_string(),
            content_preview: preview,
            timestamp,
            model,
            stop_reason,
            input_tokens: usage.and_then(|u| u.get("input")).and_then(|v| v.as_u64()),
            output_tokens: usage.and_then(|u| u.get("output")).and_then(|v| v.as_u64()),
            cache_read_input_tokens: usage
                .and_then(|u| u.get("cacheRead"))
                .and_then(|v| v.as_u64()),
            cache_creation_input_tokens: usage
                .and_then(|u| u.get("cacheWrite"))
                .and_then(|v| v.as_u64()),
        });
    }
    if out.len() > count {
        out.split_off(out.len() - count)
    } else {
        out
    }
}

pub fn extract_token_totals(entries: &[Value]) -> (u64, u64) {
    let mut total_input = 0u64;
    let mut total_output = 0u64;
    for entry in entries {
        if message_role(entry) != Some("assistant") {
            continue;
        }
        if let Some(usage) = entry.get("message").and_then(|m| m.get("usage")) {
            total_input += usage.get("input").and_then(|v| v.as_u64()).unwrap_or(0);
            total_input += usage.get("cacheRead").and_then(|v| v.as_u64()).unwrap_or(0);
            total_input += usage
                .get("cacheWrite")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            total_output += usage.get("output").and_then(|v| v.as_u64()).unwrap_or(0);
        }
    }
    (total_input, total_output)
}

pub fn explain_state(entries: &[Value], mtime_age_secs: Option<u64>) -> StateExplanation {
    let last = entries
        .iter()
        .rev()
        .find(|e| matches!(message_role(e), Some("user") | Some("assistant")));
    let (final_state, details) = match last {
        None => (
            SessionState::Idle,
            vec!["no user/assistant messages yet".to_string()],
        ),
        Some(entry) if message_role(entry) == Some("user") => (
            SessionState::Processing,
            vec!["last meaningful message is user → agent is working".to_string()],
        ),
        Some(entry) => {
            let stop = assistant_stop_reason(entry).unwrap_or("");
            let state = match stop {
                "toolUse" => SessionState::Processing,
                "stop" | "error" | "aborted" | "length" => SessionState::WaitingForInput,
                _ => SessionState::Processing,
            };
            (
                state.clone(),
                vec![format!("last assistant stopReason={:?} → {}", stop, state)],
            )
        }
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
            kind: entry
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("?")
                .to_string(),
            timestamp: entry
                .get("timestamp")
                .and_then(|t| t.as_str())
                .map(str::to_string),
            stop_reason: assistant_stop_reason(entry).map(str::to_string),
            blocks: summarize_blocks(entry),
        })
        .collect();

    StateExplanation {
        final_state: final_state.clone(),
        mtime_age_secs,
        entry_count: entries.len(),
        steps: vec![ExplanationStep {
            name: "pi_last_meaningful_message",
            verdict: Verdict::Decided(final_state),
            details,
        }],
        tail,
    }
}

fn summarize_blocks(entry: &Value) -> Vec<String> {
    let Some(arr) = entry
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|block| match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => Some("text".to_string()),
            Some("thinking") => Some("thinking".to_string()),
            Some("toolCall") => block
                .get("name")
                .and_then(|n| n.as_str())
                .map(|n| format!("tool:{}", n)),
            _ => None,
        })
        .collect()
}

fn extract_text_content(entry: &Value) -> String {
    match message_role(entry) {
        Some("user") => {
            let Some(content) = entry.get("message").and_then(|m| m.get("content")) else {
                return NO_CONTENT.to_string();
            };
            content_text(content, 200).unwrap_or_else(|| "(complex content)".to_string())
        }
        Some("assistant") => {
            let Some(arr) = entry
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            else {
                return NO_CONTENT.to_string();
            };
            let mut parts = Vec::new();
            for block in arr {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            parts.push(truncate_str(text, 200));
                        }
                    }
                    Some("toolCall") => {
                        parts.push(format!("{}{}]", TOOL_MARKER_PREFIX, tool_display(block)));
                    }
                    Some("thinking") => parts.push(THINKING_MARKER.to_string()),
                    _ => {}
                }
            }
            if parts.is_empty() {
                NO_TEXT_CONTENT.to_string()
            } else {
                parts.join(" ")
            }
        }
        Some("toolResult") => entry
            .get("message")
            .and_then(|m| m.get("toolName"))
            .and_then(|v| v.as_str())
            .map(|name| format!("[tool result: {}]", name))
            .unwrap_or_else(|| "[tool result]".to_string()),
        _ => "(unknown)".to_string(),
    }
}

fn tool_display(block: &Value) -> String {
    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("?");
    let Some(raw) = block.get("arguments").and_then(|a| tool_brief_arg(name, a)) else {
        return name.to_string();
    };
    let cleaned = raw.replace(']', ")");
    let brief: String = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = brief
        .char_indices()
        .nth(60)
        .map(|(i, _)| i)
        .unwrap_or(brief.len());
    if end < brief.len() {
        format!("{}({}…)", name, &brief[..end])
    } else {
        format!("{}({})", name, brief)
    }
}

fn tool_brief_arg(name: &str, args: &Value) -> Option<String> {
    let s = |key: &str| args.get(key).and_then(|v| v.as_str()).map(str::to_string);
    match name {
        "bash" => s("command"),
        "read" | "write" | "edit" | "ls" => s("path"),
        "grep" => s("pattern"),
        _ => args
            .as_object()
            .and_then(|o| o.values().find_map(|v| v.as_str().map(str::to_string))),
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    crate::models::first_line_truncated(s.trim(), max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write as _;

    // --- helper constructors (mirror conversation.rs json! fixture idioms) ---

    /// Pi user entry: `type=message`, `message.role=user`, string or array content.
    fn user(content: Value) -> Value {
        json!({"type": "message", "message": {"role": "user", "content": content}})
    }

    /// Pi assistant entry with the given stopReason and content blocks.
    fn assistant(stop_reason: &str, content: Value) -> Value {
        json!({"type": "message", "message": {
            "role": "assistant", "stopReason": stop_reason, "content": content
        }})
    }

    /// Pi `toolCall` content block.
    fn tool_call(id: &str, name: &str, arguments: Value) -> Value {
        json!({"type": "toolCall", "id": id, "name": name, "arguments": arguments})
    }

    /// Pi `toolResult` entry resolving the given toolCallId.
    fn tool_result(id: &str) -> Value {
        json!({"type": "message", "message": {"role": "toolResult", "toolCallId": id}})
    }

    /// Write JSONL lines to a fresh tempfile and return the handle (keep it
    /// alive for the test so the file isn't unlinked).
    fn write_jsonl(lines: &[&str]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().expect("create tempfile");
        for line in lines {
            writeln!(f, "{}", line).expect("write line");
        }
        f.flush().expect("flush");
        f
    }

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

    // --- extract_current_tool ---

    #[test]
    fn extract_current_tool_none_when_empty() {
        assert_eq!(extract_current_tool(&[]), None);
    }

    #[test]
    fn extract_current_tool_returns_unresolved_among_parallel_calls() {
        let entries = vec![
            assistant(
                "toolUse",
                json!([
                    tool_call("t1", "bash", json!({"command": "ls"})),
                    tool_call("t2", "edit", json!({"path": "a.rs"})),
                ]),
            ),
            tool_result("t1"),
        ];
        let got = extract_current_tool(&entries).unwrap();
        assert_eq!(got.name, "edit");
    }

    #[test]
    fn extract_current_tool_none_when_all_resolved() {
        let entries = vec![
            assistant(
                "toolUse",
                json!([tool_call("t1", "read", json!({"path": "x"}))]),
            ),
            tool_result("t1"),
        ];
        assert_eq!(extract_current_tool(&entries), None);
    }

    #[test]
    fn extract_current_tool_prefers_most_recent_unresolved() {
        let entries = vec![
            assistant(
                "toolUse",
                json!([tool_call("old", "bash", json!({"command": "a"}))]),
            ),
            assistant(
                "toolUse",
                json!([tool_call("new", "grep", json!({"pattern": "foo"}))]),
            ),
        ];
        assert_eq!(extract_current_tool(&entries).unwrap().name, "grep");
    }

    #[test]
    fn extract_current_tool_bash_command_hint() {
        let entries = vec![assistant(
            "toolUse",
            json!([tool_call(
                "t1",
                "bash",
                json!({"command": "cargo  build   --release"})
            )]),
        )];
        let got = extract_current_tool(&entries).unwrap();
        assert_eq!(got.name, "bash");
        // Whitespace is collapsed to single spaces.
        assert_eq!(got.hint.as_deref(), Some("cargo build --release"));
    }

    #[test]
    fn extract_current_tool_path_hint_for_file_tools() {
        let entries = vec![assistant(
            "toolUse",
            json!([tool_call(
                "t1",
                "read",
                json!({"path": "/home/u/proj/main.rs"})
            )]),
        )];
        // Pi's format_tool_hint does NOT take the basename (unlike Claude) —
        // it returns the whole path with whitespace collapsed.
        assert_eq!(
            extract_current_tool(&entries).unwrap().hint.as_deref(),
            Some("/home/u/proj/main.rs")
        );
    }

    #[test]
    fn extract_current_tool_grep_pattern_hint() {
        let entries = vec![assistant(
            "toolUse",
            json!([tool_call("t1", "grep", json!({"pattern": "TODO"}))]),
        )];
        assert_eq!(
            extract_current_tool(&entries).unwrap().hint.as_deref(),
            Some("TODO")
        );
    }

    #[test]
    fn extract_current_tool_unknown_tool_uses_first_string_arg() {
        // The `_` arm falls back to the first string-valued argument.
        let entries = vec![assistant(
            "toolUse",
            json!([tool_call(
                "t1",
                "mystery",
                json!({"n": 1, "label": "hello"})
            )]),
        )];
        assert_eq!(
            extract_current_tool(&entries).unwrap().hint.as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn extract_current_tool_skips_block_missing_id_or_name() {
        // A toolCall with empty id/name is ignored (can't be resolved).
        let entries = vec![assistant(
            "toolUse",
            json!([json!({"type": "toolCall", "arguments": {"command": "x"}})]),
        )];
        assert_eq!(extract_current_tool(&entries), None);
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

    // --- extract_last_user_message / extract_first_user_message ---

    #[test]
    fn extract_last_user_message_string_content() {
        let entries = vec![
            user(json!("first")),
            assistant("stop", json!([{"type": "text", "text": "reply"}])),
            user(json!("second")),
        ];
        assert_eq!(
            extract_last_user_message(&entries).as_deref(),
            Some("second")
        );
    }

    #[test]
    fn extract_first_user_message_string_content() {
        let entries = vec![user(json!("first")), user(json!("second"))];
        assert_eq!(
            extract_first_user_message(&entries).as_deref(),
            Some("first")
        );
    }

    #[test]
    fn extract_user_message_array_text_block() {
        let entries = vec![user(json!([
            {"type": "text", "text": "hello from array"}
        ]))];
        assert_eq!(
            extract_last_user_message(&entries).as_deref(),
            Some("hello from array")
        );
    }

    #[test]
    fn extract_user_message_none_when_no_user_entries() {
        let entries = vec![assistant("stop", json!([{"type": "text", "text": "x"}]))];
        assert_eq!(extract_last_user_message(&entries), None);
        assert_eq!(extract_first_user_message(&entries), None);
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

    // --- extract_messages ---

    #[test]
    fn extract_messages_maps_roles_and_usage() {
        let entries = vec![
            json!({"type": "message", "timestamp": 1000u64,
                "message": {"role": "user", "content": "hello"}}),
            json!({"type": "message", "timestamp": 2000u64,
                "message": {"role": "assistant", "stopReason": "stop",
                    "model": "claude-opus-4",
                    "content": [{"type": "text", "text": "hi there"}],
                    "usage": {"input": 10, "output": 5, "cacheRead": 2, "cacheWrite": 1}}}),
        ];
        let msgs = extract_messages(&entries, 10);
        assert_eq!(msgs.len(), 2);

        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content_preview, "hello");
        assert_eq!(msgs[0].timestamp, 1000);
        assert_eq!(msgs[0].model, None);
        assert_eq!(msgs[0].input_tokens, None);

        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].content_preview, "hi there");
        assert_eq!(msgs[1].timestamp, 2000);
        assert_eq!(msgs[1].model.as_deref(), Some("claude-opus-4"));
        assert_eq!(msgs[1].stop_reason.as_deref(), Some("stop"));
        assert_eq!(msgs[1].input_tokens, Some(10));
        assert_eq!(msgs[1].output_tokens, Some(5));
        assert_eq!(msgs[1].cache_read_input_tokens, Some(2));
        assert_eq!(msgs[1].cache_creation_input_tokens, Some(1));
    }

    #[test]
    fn extract_messages_keeps_last_n() {
        let entries = vec![user(json!("one")), user(json!("two")), user(json!("three"))];
        let msgs = extract_messages(&entries, 2);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content_preview, "two");
        assert_eq!(msgs[1].content_preview, "three");
    }

    #[test]
    fn extract_messages_skips_non_message_entries() {
        let entries = vec![
            json!({"type": "model_change", "modelId": "x"}),
            user(json!("real")),
        ];
        let msgs = extract_messages(&entries, 10);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content_preview, "real");
    }

    #[test]
    fn extract_messages_assistant_tool_call_preview() {
        let entries = vec![assistant(
            "toolUse",
            json!([tool_call("t1", "bash", json!({"command": "ls -la"}))]),
        )];
        let msgs = extract_messages(&entries, 10);
        assert_eq!(msgs.len(), 1);
        assert!(
            msgs[0].content_preview.contains("bash"),
            "tool preview should name the tool, got {:?}",
            msgs[0].content_preview
        );
    }

    // --- extract_token_totals ---

    #[test]
    fn extract_token_totals_sums_only_assistant_usage() {
        let entries = vec![
            json!({"type": "message", "message": {"role": "assistant",
                "usage": {"input": 100, "cacheRead": 10, "cacheWrite": 5, "output": 20}}}),
            // A user entry carrying a usage block must NOT be counted: the Pi
            // tally is gated on message.role == assistant.
            json!({"type": "message", "message": {"role": "user",
                "usage": {"input": 999, "output": 999}}}),
            json!({"type": "message", "message": {"role": "assistant",
                "usage": {"input": 200, "output": 30}}}),
        ];
        let (input, output) = extract_token_totals(&entries);
        assert_eq!(input, 100 + 10 + 5 + 200);
        assert_eq!(output, 20 + 30);
    }

    #[test]
    fn extract_token_totals_zero_when_no_usage() {
        let entries = vec![user(json!("hi"))];
        assert_eq!(extract_token_totals(&entries), (0, 0));
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

    // --- count_tool_uses_in_reader ---

    #[test]
    fn count_tool_uses_counts_pi_tool_call_blocks() {
        let data = concat!(
            r#"{"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"a","name":"bash"},{"type":"toolCall","id":"b","name":"read"}]}}"#,
            "\n",
            r#"{"type":"message","message":{"role":"user","content":"hi"}}"#,
            "\n",
            r#"{"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"c","name":"grep"}]}}"#,
            "\n",
        );
        assert_eq!(count_tool_uses_in_reader(data.as_bytes()), 3);
    }

    #[test]
    fn count_tool_uses_ignores_non_assistant_tool_calls() {
        // toolCall blocks only count under an assistant message.
        let data = concat!(
            r#"{"type":"message","message":{"role":"user","content":[{"type":"toolCall","id":"a","name":"bash"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"toolCall","id":"b","name":"read"}]}}"#,
            "\n",
        );
        assert_eq!(count_tool_uses_in_reader(data.as_bytes()), 0);
    }
}
