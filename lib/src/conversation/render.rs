//! Text rendering helpers: content previews, tool display, XML-tag stripping.

use serde_json::Value;

pub(crate) const NO_CONTENT: &str = "(no content)";
pub(crate) const NO_TEXT_CONTENT: &str = "(no text content)";
pub(crate) const TOOL_MARKER_PREFIX: &str = "[tool: ";
pub(crate) const THINKING_MARKER: &str = "[thinking...]";

pub(super) fn extract_text_content(entry: &Value) -> String {
    let msg_type = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");

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
            NO_CONTENT.to_string()
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
                                parts.push(format!(
                                    "{}{}]",
                                    TOOL_MARKER_PREFIX,
                                    tool_display(block)
                                ));
                            }
                            Some("thinking") => {
                                parts.push(THINKING_MARKER.to_string());
                            }
                            _ => {}
                        }
                    }
                    if parts.is_empty() {
                        return NO_TEXT_CONTENT.to_string();
                    }
                    return parts.join(" ");
                }
                if let Some(text) = content.as_str() {
                    return truncate_str(text, 200);
                }
            }
            NO_CONTENT.to_string()
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

fn tool_display(block: &serde_json::Value) -> String {
    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("?");
    let Some(raw) = block.get("input").and_then(|i| tool_brief_arg(name, i)) else {
        return name.to_string();
    };
    // `]` would terminate the surrounding `[tool: ...]` marker; whitespace
    // collapses to single spaces so the display fits on one line.
    let cleaned = raw.replace(']', ")");
    let brief: String = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut end = brief.len().min(60);
    while end > 0 && !brief.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        name.to_string()
    } else if end < brief.len() {
        format!("{}({}…)", name, &brief[..end])
    } else {
        format!("{}({})", name, brief)
    }
}

fn tool_brief_arg(name: &str, input: &serde_json::Value) -> Option<String> {
    let s = |key: &str| {
        input
            .get(key)
            .and_then(|v| v.as_str())
            .map(|v| v.to_string())
    };
    match name {
        "Bash" => s("command"),
        "Read" | "Edit" | "Write" | "NotebookEdit" | "MultiEdit" => s("file_path"),
        "Glob" => s("pattern"),
        "Grep" => {
            let pat = s("pattern")?;
            match s("glob") {
                Some(g) if !g.is_empty() => Some(format!("{} in {}", pat, g)),
                _ => Some(pat),
            }
        }
        "WebFetch" => s("url"),
        "WebSearch" => s("query"),
        "Task" | "Agent" => s("description").or_else(|| s("subagent_type")),
        "TodoWrite" | "TaskCreate" | "TaskUpdate" => s("title").or_else(|| s("description")),
        _ => {
            // Generic fallback: first string-valued field in the input object.
            input
                .as_object()
                .and_then(|obj| obj.values().find_map(|v| v.as_str().map(String::from)))
        }
    }
}

pub(super) fn truncate_str(s: &str, max: usize) -> String {
    let s = strip_xml_tags(s);
    crate::models::first_line_truncated(s.trim(), max)
}
