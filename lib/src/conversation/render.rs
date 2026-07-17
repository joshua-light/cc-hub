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
///
/// Only spans that plausibly are such tags are removed: `<`, an optional `/`,
/// a leading letter, then tag-name chars `[-_A-Za-z0-9]`, closed by `>`. A `<`
/// that doesn't start such a tag is emitted verbatim, so prose and inequalities
/// like `why is a < b here?` and `x < 5 and y > 3` survive intact.
fn strip_xml_tags(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            if let Some(close) = tag_close_index(&chars, i) {
                i = close + 1; // skip the whole `<...>`
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// If `chars[start] == '<'` begins a plausible leaked tag, return the index of
/// its closing `>`. Otherwise `None`, meaning the `<` is ordinary text.
fn tag_close_index(chars: &[char], start: usize) -> Option<usize> {
    let mut j = start + 1;
    if chars.get(j) == Some(&'/') {
        j += 1;
    }
    // First char after the optional `/` must be a letter.
    match chars.get(j) {
        Some(c) if c.is_ascii_alphabetic() => j += 1,
        _ => return None,
    }
    // Remaining tag-name chars, closed by `>`. Anything else (whitespace,
    // punctuation) means this isn't a tag.
    while let Some(&c) = chars.get(j) {
        if c == '>' {
            return Some(j);
        }
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            j += 1;
        } else {
            return None;
        }
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_xml_tags_removes_leaked_tags() {
        assert_eq!(strip_xml_tags("<bash-stdout>ok</bash-stdout>"), "ok");
        assert_eq!(
            strip_xml_tags("before<system-reminder>after"),
            "beforeafter"
        );
    }

    #[test]
    fn strip_xml_tags_keeps_inequalities_and_prose() {
        // A `<` with no matching tag must not swallow the rest of the string.
        assert_eq!(strip_xml_tags("why is a < b here?"), "why is a < b here?");
        // A non-tag `<`…`>` pair must be left untouched.
        assert_eq!(strip_xml_tags("x < 5 and y > 3"), "x < 5 and y > 3");
    }

    #[test]
    fn strip_xml_tags_handles_dangling_and_empty_angles() {
        assert_eq!(strip_xml_tags("a <> b"), "a <> b");
        assert_eq!(strip_xml_tags("trailing <"), "trailing <");
    }
}
