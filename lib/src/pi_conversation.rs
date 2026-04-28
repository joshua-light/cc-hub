use crate::conversation::{
    parse_timestamp_ms, CurrentTool, EntrySummary, ExplanationStep, StateExplanation, Verdict,
    NO_CONTENT, NO_TEXT_CONTENT, THINKING_MARKER, TOOL_MARKER_PREFIX,
};
use crate::models::{ConversationMessage, SessionState};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

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
    let s = s.trim();
    let first_line = s.lines().next().unwrap_or(s);
    if first_line.chars().count() <= max {
        first_line.to_string()
    } else {
        let mut out = String::new();
        for c in first_line.chars().take(max) {
            out.push(c);
        }
        out.push_str("...");
        out
    }
}
