use super::message_role;
use crate::conversation::CurrentTool;
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
/// Shared with [`crate::conversation::tool_count`] for incremental updates.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::pi::test_util::*;
    use serde_json::json;

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
