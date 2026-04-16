use crate::models::{ConversationMessage, SessionState};
use log::debug;
use serde_json::Value;
use std::collections::HashSet;
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

pub fn read_jsonl_head(path: &Path, max_bytes: u64) -> Vec<Value> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let len = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return Vec::new(),
    };

    let read_len = len.min(max_bytes);
    let mut buf = vec![0u8; read_len as usize];
    if std::io::Read::read_exact(&mut file, &mut buf).is_err() {
        return Vec::new();
    }

    let text = String::from_utf8_lossy(&buf);
    let mut lines = Vec::new();
    let line_iter: Vec<&str> = text.lines().collect();
    let last_idx = if len > max_bytes {
        // File was truncated — discard last (potentially partial) line
        line_iter.len().saturating_sub(1)
    } else {
        line_iter.len()
    };

    for line in &line_iter[..last_idx] {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<Value>(line) {
            lines.push(val);
        }
    }

    lines
}

/// Check if a content Value (string or array) contains a `<command-name>` tag,
/// which indicates a local slash command (/clear, /compact, etc.).
fn content_contains_command_name(content: &Value) -> bool {
    if let Some(text) = content.as_str() {
        if text.contains("<command-name>") {
            return true;
        }
    }
    if let Some(arr) = content.as_array() {
        for block in arr {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    if text.contains("<command-name>") {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn is_meaningful_entry(entry: &Value) -> bool {
    // Skip metadata entries (e.g. local-command-caveat after /clear).
    if entry.get("isMeta").and_then(|v| v.as_bool()) == Some(true) {
        return false;
    }

    match entry.get("type").and_then(|t| t.as_str()) {
        Some("user") => {
            if let Some(content) = entry.get("message").and_then(|m| m.get("content")) {
                // Skip tool_result messages (auto-generated, not real user input)
                if let Some(arr) = content.as_array() {
                    let only_tool_results = arr.iter().all(|b| {
                        b.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                    });
                    if only_tool_results && !arr.is_empty() {
                        return false;
                    }
                }
                // Skip local slash commands (/clear, /compact, etc.)
                if content_contains_command_name(content) {
                    return false;
                }
            }
            true
        }
        Some("assistant") => true,
        Some("system") => {
            // Skip local_command entries (/clear, /compact, etc.)
            let sub = entry.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
            sub != "local_command"
        }
        _ => false,
    }
}

/// Tools that block on user interaction (permission prompts, plan mode, etc.).
/// When the assistant's last action is calling one of these, the session is
/// waiting for user input, not actively processing.
const USER_INPUT_TOOLS: &[&str] = &[
    "EnterPlanMode",
    "ExitPlanMode",
    "AskUserQuestion",
];

/// Returns true if the assistant message contains a tool_use block for a tool
/// that requires user interaction.
fn assistant_awaits_user_input(entry: &Value) -> bool {
    let content = match entry
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        Some(arr) => arr,
        None => return false,
    };
    content.iter().any(|block| {
        block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
            && block
                .get("name")
                .and_then(|n| n.as_str())
                .is_some_and(|name| USER_INPUT_TOOLS.contains(&name))
    })
}

/// Returns true if there's a dangling assistant `tool_use` (no matching
/// `tool_result`) AND a `last-prompt` entry appears after it — the signature
/// of an interrupted turn where the user typed a new message. A dangling
/// `tool_use` alone can just mean the tool is still running, so we require
/// the `last-prompt` marker to disambiguate.
fn interrupted_tool_use(entries: &[Value]) -> bool {
    let mut unresolved: Vec<(usize, &str)> = Vec::new();
    let mut results: HashSet<&str> = HashSet::new();
    let mut last_prompt_idx: Option<usize> = None;

    for (i, entry) in entries.iter().enumerate() {
        let t = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if t == "last-prompt" {
            last_prompt_idx = Some(i);
            continue;
        }
        let arr = match entry
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        {
            Some(a) => a,
            None => continue,
        };
        for block in arr {
            let bt = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if t == "assistant" && bt == "tool_use" {
                if let Some(id) = block.get("id").and_then(|v| v.as_str()) {
                    unresolved.push((i, id));
                }
            } else if t == "user" && bt == "tool_result" {
                if let Some(id) = block.get("tool_use_id").and_then(|v| v.as_str()) {
                    results.insert(id);
                }
            }
        }
    }

    let Some(lp_idx) = last_prompt_idx else {
        return false;
    };
    unresolved
        .iter()
        .any(|(idx, id)| *idx < lp_idx && !results.contains(id))
}

/// Returns true if the last non-metadata entry is a user `tool_result`
/// (meaning the agent's tool just finished and the next assistant response
/// hasn't been written yet).
fn last_entry_is_tool_result(entries: &[Value]) -> bool {
    for entry in entries.iter().rev() {
        match entry.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "assistant" => return false,
            "user" => {
                return entry
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                    .is_some_and(|arr| {
                        !arr.is_empty()
                            && arr.iter().all(|b| {
                                b.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                            })
                    });
            }
            _ => continue,
        }
    }
    false
}

/// Determine session state from the last meaningful user/assistant entry.
///   Processing      — last entry indicates the agent is working
///   WaitingForInput — last entry indicates a completed turn
///   Idle            — no meaningful entries (fresh session)
///
/// System entries (turn_duration, stop_hook_summary, etc.) are metadata that
/// appear between turns.  They must be skipped for state detection because
/// during an active tool-use loop a `turn_duration` entry sits between the
/// assistant's tool_use request and the tool result, causing a false
/// WaitingForInput while the agent is actually executing a tool.
pub fn extract_state(entries: &[Value], mtime_age_secs: Option<u64>) -> SessionState {
    // 1. Interrupted turn: dangling tool_use + trailing `last-prompt` marker
    //    indicates the user hit Esc mid-tool and typed a new message — the
    //    session is waiting for them to submit it.
    if interrupted_tool_use(entries) {
        debug!("extract_state: interrupted tool_use (last-prompt follows) → WaitingForInput");
        return SessionState::WaitingForInput;
    }

    // 2. Trailing tool_result with stale mtime → agent finished a tool but
    //    hasn't written its next response. If the file's been idle for more
    //    than the threshold, it's almost certainly blocked on the next tool's
    //    permission prompt (not yet serialized to JSONL).
    const STALE_TOOL_RESULT_SECS: u64 = 30;
    if last_entry_is_tool_result(entries)
        && mtime_age_secs.is_some_and(|age| age >= STALE_TOOL_RESULT_SECS)
    {
        debug!(
            "extract_state: stale tool_result (mtime age {:?}s) → WaitingForInput",
            mtime_age_secs
        );
        return SessionState::WaitingForInput;
    }

    let last = match entries
        .iter()
        .rev()
        .filter(|e| is_meaningful_entry(e))
        .find(|e| {
            matches!(
                e.get("type").and_then(|t| t.as_str()),
                Some("user") | Some("assistant")
            )
        }) {
        Some(e) => e,
        None => {
            debug!("extract_state: no meaningful user/assistant entry → Idle");
            return SessionState::Idle;
        }
    };

    let entry_type = last.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let state = match entry_type {
        "user" => SessionState::Processing,
        "assistant" => {
            let stop = last
                .get("message")
                .and_then(|m| m.get("stop_reason"))
                .and_then(|s| s.as_str())
                .unwrap_or("");
            match stop {
                "end_turn" => SessionState::WaitingForInput,
                // tool_use means the agent requested a tool call.
                // Some tools block on user interaction — treat those as
                // WaitingForInput so the UI shows them correctly.
                "tool_use" => {
                    let awaits = assistant_awaits_user_input(last);
                    debug!(
                        "extract_state: last=assistant stop=tool_use awaits_input={}",
                        awaits
                    );
                    if awaits {
                        SessionState::WaitingForInput
                    } else {
                        SessionState::Processing
                    }
                }
                _ => {
                    debug!("extract_state: last=assistant stop_reason={:?} → WaitingForInput", stop);
                    SessionState::WaitingForInput
                }
            }
        }
        _ => SessionState::WaitingForInput,
    };

    debug!("extract_state: last_type={} → {}", entry_type, state);
    state
}

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

/// Strip XML-like tags (e.g. `<bash-stdout>`, `<system-reminder>`) that leak
/// from Claude Code's internal JSONL format.
fn strip_xml_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            // Consume everything up to and including the closing '>'
            let mut found_close = false;
            for inner in chars.by_ref() {
                if inner == '>' {
                    found_close = true;
                    break;
                }
            }
            if !found_close {
                // Malformed — put the '<' back
                out.push('<');
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn truncate_str(s: &str, max: usize) -> String {
    let s = strip_xml_tags(s);
    let s = s.trim();
    let first_line = s.lines().next().unwrap_or(s);
    if first_line.len() <= max {
        first_line.to_string()
    } else {
        // Find a char boundary at or before `max` to avoid splitting multi-byte chars.
        let mut end = max.min(first_line.len());
        while end > 0 && !first_line.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &first_line[..end])
    }
}
