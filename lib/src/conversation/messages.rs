//! Message and metadata extraction from JSONL entries.

use super::render::{extract_text_content, truncate_str};
use super::state::is_meaningful_entry;
use crate::models::ConversationMessage;
use serde_json::Value;

pub fn extract_last_user_message(entries: &[Value]) -> Option<String> {
    entries
        .iter()
        .rev()
        .filter(|e| is_meaningful_entry(e))
        .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("user"))
        .find_map(|e| extract_user_text(e, 120))
}

pub fn extract_first_user_message(entries: &[Value]) -> Option<String> {
    entries
        .iter()
        .filter(|e| is_meaningful_entry(e))
        .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("user"))
        .find_map(|e| extract_user_text(e, 200))
}

/// Extract text from a user message entry, handling both string and array content.
fn extract_user_text(entry: &Value, max_len: usize) -> Option<String> {
    let content = entry.get("message")?.get("content")?;
    if let Some(text) = content.as_str() {
        if !text.is_empty() {
            return Some(truncate_str(text, max_len));
        }
    }
    if let Some(arr) = content.as_array() {
        for block in arr {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    if !text.is_empty() {
                        return Some(truncate_str(text, max_len));
                    }
                }
            }
        }
    }
    None
}

pub fn extract_metadata(entries: &[Value]) -> (Option<String>, Option<String>, Option<String>) {
    let mut git_branch = None;
    let mut model = None;
    let mut version = None;

    for entry in entries.iter().rev() {
        if git_branch.is_none() {
            if let Some(b) = entry.get("gitBranch").and_then(|v| v.as_str()) {
                git_branch = Some(b.to_string());
            }
        }
        if model.is_none() {
            if let Some(m) = entry
                .get("message")
                .and_then(|msg| msg.get("model"))
                .and_then(|v| v.as_str())
            {
                model = Some(m.to_string());
            }
        }
        if version.is_none() {
            if let Some(v) = entry.get("version").and_then(|v| v.as_str()) {
                version = Some(v.to_string());
            }
        }
        if git_branch.is_some() && model.is_some() && version.is_some() {
            break;
        }
    }

    (git_branch, model, version)
}

pub fn extract_last_activity(entries: &[Value]) -> Option<u64> {
    entries
        .iter()
        .rev()
        .find_map(|e| e.get("timestamp").and_then(parse_timestamp_ms))
}

/// Parse a JSONL timestamp field to epoch milliseconds.
/// Handles both integer timestamps and ISO 8601 strings (e.g. "2026-04-15T18:14:30.201Z").
pub fn parse_timestamp_ms(val: &Value) -> Option<u64> {
    if let Some(n) = val.as_u64() {
        return Some(n);
    }
    if let Some(s) = val.as_str() {
        return parse_iso8601_ms(s);
    }
    None
}

fn parse_iso8601_ms(s: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis() as u64)
}

pub fn extract_messages(entries: &[Value], count: usize) -> Vec<ConversationMessage> {
    let mut msgs: Vec<ConversationMessage> = entries
        .iter()
        .rev()
        .filter(|e| is_meaningful_entry(e))
        .take(count)
        .map(|e| {
            let role = e
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("unknown")
                .to_string();

            let content_preview = extract_text_content(e);
            let timestamp = e.get("timestamp").and_then(parse_timestamp_ms).unwrap_or(0);

            let model = e
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let stop_reason = e
                .get("message")
                .and_then(|m| m.get("stop_reason"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let usage_u64 = |field: &str| -> Option<u64> {
                e.get("message")
                    .and_then(|m| m.get("usage"))
                    .and_then(|u| u.get(field))
                    .and_then(|v| v.as_u64())
            };
            let input_tokens = usage_u64("input_tokens");
            let output_tokens = usage_u64("output_tokens");
            let cache_read_input_tokens = usage_u64("cache_read_input_tokens");
            let cache_creation_input_tokens = usage_u64("cache_creation_input_tokens");

            ConversationMessage {
                role,
                content_preview,
                timestamp,
                model,
                stop_reason,
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
            }
        })
        .collect::<Vec<_>>();
    msgs.reverse();
    msgs
}

pub fn extract_token_totals(entries: &[Value]) -> (u64, u64) {
    let mut total_input = 0u64;
    let mut total_output = 0u64;

    for entry in entries {
        if let Some(usage) = entry.get("message").and_then(|m| m.get("usage")) {
            if let Some(input) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                total_input += input;
            }
            if let Some(cache_create) = usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
            {
                total_input += cache_create;
            }
            if let Some(cache_read) = usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
            {
                total_input += cache_read;
            }
            if let Some(output) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                total_output += output;
            }
        }
    }

    (total_input, total_output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_iso8601_with_millis() {
        let ms = parse_iso8601_ms("2026-04-15T18:14:30.201Z").unwrap();
        // 2026-04-15T18:14:30.201Z
        assert_eq!(ms % 1000, 201);
        assert!(ms > 1_776_000_000_000); // sanity: after 2026
    }

    #[test]
    fn parse_iso8601_no_millis() {
        let ms = parse_iso8601_ms("2026-04-15T18:14:30Z").unwrap();
        assert_eq!(ms % 1000, 0);
    }

    #[test]
    fn parse_timestamp_ms_integer() {
        let val = serde_json::json!(1776271524302u64);
        assert_eq!(parse_timestamp_ms(&val), Some(1776271524302));
    }

    #[test]
    fn parse_timestamp_ms_string() {
        let val = serde_json::json!("2026-04-15T18:14:30.201Z");
        let ms = parse_timestamp_ms(&val).unwrap();
        assert_eq!(ms % 1000, 201);
    }

    #[test]
    fn extract_last_activity_with_iso_timestamps() {
        let entries = vec![
            serde_json::json!({"type": "user", "timestamp": "2026-04-15T18:14:00.000Z"}),
            serde_json::json!({"type": "assistant", "timestamp": "2026-04-15T18:14:30.201Z"}),
        ];
        let result = extract_last_activity(&entries);
        assert!(result.is_some());
        assert_eq!(result.unwrap() % 1000, 201);
    }
}
