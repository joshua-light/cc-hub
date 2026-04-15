use crate::models::{ConversationMessage, SessionState};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

pub fn read_jsonl_tail(path: &Path, max_bytes: u64) -> Vec<Value> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let len = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return Vec::new(),
    };

    let seek_pos = if len > max_bytes { len - max_bytes } else { 0 };
    if file.seek(SeekFrom::Start(seek_pos)).is_err() {
        return Vec::new();
    }

    let reader = BufReader::new(&mut file);
    let mut lines = Vec::new();
    let mut first = seek_pos > 0;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if first {
            first = false;
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<Value>(&line) {
            lines.push(val);
        }
    }

    lines
}

fn is_meaningful_entry(entry: &Value) -> bool {
    match entry.get("type").and_then(|t| t.as_str()) {
        Some("user") => {
            // Skip tool_result messages (auto-generated, not real user input)
            if let Some(content) = entry.get("message").and_then(|m| m.get("content")) {
                if let Some(arr) = content.as_array() {
                    let only_tool_results = arr.iter().all(|b| {
                        b.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                    });
                    if only_tool_results && !arr.is_empty() {
                        return false;
                    }
                }
            }
            true
        }
        Some("assistant") | Some("system") => true,
        _ => false,
    }
}

pub fn extract_state(entries: &[Value]) -> SessionState {
    let last = entries.iter().rev().find(|e| is_meaningful_entry(e));

    match last {
        None => SessionState::Idle,
        Some(entry) => {
            let msg_type = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match msg_type {
                "assistant" => {
                    let stop_reason = entry
                        .get("message")
                        .and_then(|m| m.get("stop_reason"))
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    match stop_reason {
                        "end_turn" => SessionState::WaitingForInput,
                        "tool_use" => SessionState::ToolExecution,
                        _ => SessionState::Idle,
                    }
                }
                "user" => SessionState::Processing,
                "system" => {
                    let subtype = entry.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
                    if subtype == "turn_duration" {
                        SessionState::WaitingForInput
                    } else {
                        SessionState::Idle
                    }
                }
                _ => SessionState::Idle,
            }
        }
    }
}

pub fn extract_last_user_message(entries: &[Value]) -> Option<String> {
    entries
        .iter()
        .rev()
        .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("user"))
        .find_map(|e| {
            let content = e.get("message")?.get("content")?;
            if let Some(text) = content.as_str() {
                if !text.is_empty() {
                    let truncated = truncate_str(text, 120);
                    return Some(truncated);
                }
            }
            None
        })
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
        .find_map(|e| e.get("timestamp").and_then(|t| t.as_u64()))
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
            let timestamp = e.get("timestamp").and_then(|t| t.as_u64()).unwrap_or(0);

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

            let input_tokens = e
                .get("message")
                .and_then(|m| m.get("usage"))
                .and_then(|u| u.get("input_tokens"))
                .and_then(|v| v.as_u64());

            let output_tokens = e
                .get("message")
                .and_then(|m| m.get("usage"))
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_u64());

            ConversationMessage {
                role,
                content_preview,
                timestamp,
                model,
                stop_reason,
                input_tokens,
                output_tokens,
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

fn extract_text_content(entry: &Value) -> String {
    let msg_type = entry
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("");

    match msg_type {
        "user" => {
            if let Some(content) = entry.get("message").and_then(|m| m.get("content")) {
                if let Some(text) = content.as_str() {
                    return truncate_str(text, 200);
                }
                if let Some(arr) = content.as_array() {
                    // Look for text blocks first
                    for block in arr {
                        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                return truncate_str(text, 200);
                            }
                        }
                    }
                    // If only tool_result blocks, summarize them
                    let has_tool_results = arr
                        .iter()
                        .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"));
                    if has_tool_results {
                        return "(tool result)".to_string();
                    }
                    return "(complex content)".to_string();
                }
            }
            "(no content)".to_string()
        }
        "assistant" => {
            if let Some(content) = entry.get("message").and_then(|m| m.get("content")) {
                if let Some(arr) = content.as_array() {
                    let mut parts = Vec::new();
                    for block in arr {
                        match block.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    parts.push(truncate_str(text, 200));
                                }
                            }
                            Some("tool_use") => {
                                let name = block
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("?");
                                parts.push(format!("[tool: {}]", name));
                            }
                            Some("thinking") => {
                                parts.push("[thinking...]".to_string());
                            }
                            _ => {}
                        }
                    }
                    if parts.is_empty() {
                        return "(no text content)".to_string();
                    }
                    return parts.join(" ");
                }
                if let Some(text) = content.as_str() {
                    return truncate_str(text, 200);
                }
            }
            "(no content)".to_string()
        }
        "system" => {
            let subtype = entry
                .get("subtype")
                .and_then(|s| s.as_str())
                .unwrap_or("system");
            format!("[{}]", subtype)
        }
        _ => "(unknown)".to_string(),
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    let s = s.trim();
    let first_line = s.lines().next().unwrap_or(s);
    if first_line.len() <= max {
        first_line.to_string()
    } else {
        format!("{}...", &first_line[..max.min(first_line.len())])
    }
}
