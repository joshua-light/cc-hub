use super::{content_text, message_role, message_timestamp};
use crate::conversation::render::truncate_plain;
use crate::conversation::{NO_CONTENT, NO_TEXT_CONTENT, THINKING_MARKER, TOOL_MARKER_PREFIX};
use crate::models::ConversationMessage;
use serde_json::Value;

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
                            parts.push(truncate_plain(text, 200));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::pi::test_util::*;
    use serde_json::json;

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
}
