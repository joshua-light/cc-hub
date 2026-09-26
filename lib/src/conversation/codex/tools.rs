use super::{payload, payload_type, rec_type};
use crate::conversation::render::one_line_hint;
use crate::conversation::CurrentTool;
use serde_json::Value;
use std::collections::HashSet;
use std::io::BufRead;

fn is_tool_call(entry: &Value) -> bool {
    rec_type(entry) == Some("response_item")
        && matches!(
            payload_type(entry),
            Some("function_call") | Some("custom_tool_call")
        )
}

fn is_tool_call_output(entry: &Value) -> bool {
    rec_type(entry) == Some("response_item")
        && matches!(
            payload_type(entry),
            Some("function_call_output") | Some("custom_tool_call_output")
        )
}

/// The `call_id` that pairs a `function_call`/`custom_tool_call` with its
/// `*_output`. Codex uses `call_id`; some records also carry an `id`.
fn call_id(entry: &Value) -> Option<&str> {
    let p = payload(entry)?;
    p.get("call_id")
        .or_else(|| p.get("id"))
        .and_then(|v| v.as_str())
}

/// The most recent unresolved codex tool call (a `function_call` /
/// `custom_tool_call` without a matching `*_output`) — the tool the agent is
/// currently executing.
pub fn extract_current_tool(entries: &[Value]) -> Option<CurrentTool> {
    let mut unresolved: Vec<(String, String, Option<String>)> = Vec::new();
    let mut results: HashSet<String> = HashSet::new();

    for entry in entries {
        if is_tool_call(entry) {
            let Some(p) = payload(entry) else { continue };
            let id = call_id(entry).unwrap_or("");
            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if id.is_empty() || name.is_empty() {
                continue;
            }
            let hint = format_tool_hint(name, p);
            unresolved.push((id.to_string(), name.to_string(), hint));
        } else if is_tool_call_output(entry) {
            if let Some(id) = call_id(entry) {
                results.insert(id.to_string());
            }
        }
    }

    unresolved
        .into_iter()
        .rev()
        .find(|(id, _, _)| !results.contains(id))
        .map(|(_, name, hint)| CurrentTool { name, hint })
}

/// A one-line hint for a codex tool call. `function_call` arguments are a JSON
/// *string*; `exec_command` carries the shell command under `cmd`. Falls back
/// to the first string value in the parsed arguments/input.
fn format_tool_hint(name: &str, payload: &Value) -> Option<String> {
    let args_val: Option<Value> = payload
        .get("arguments")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .or_else(|| payload.get("input").cloned());
    let args = args_val.as_ref()?;
    let raw = match name {
        "exec_command" | "shell" | "local_shell" => args
            .get("cmd")
            .or_else(|| args.get("command"))
            .and_then(value_as_command),
        _ => args
            .get("cmd")
            .or_else(|| args.get("command"))
            .and_then(value_as_command)
            .or_else(|| {
                args.as_object()
                    .and_then(|o| o.values().find_map(|v| v.as_str().map(str::to_string)))
            }),
    }?;
    one_line_hint(&raw)
}

/// A command value that may be a plain string or an argv array of strings.
fn value_as_command(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    v.as_array().map(|arr| {
        arr.iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// Streaming counter for codex tool calls (`function_call` /
/// `custom_tool_call` response items). Shared with [`crate::conversation::tool_count`].
pub fn count_tool_uses_in_reader<R: BufRead>(reader: R) -> usize {
    crate::conversation::count_blocks_in_reader(reader, |val| if is_tool_call(val) { 1 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::codex::test_util::*;
    use serde_json::json;

    // --- current tool --------------------------------------------------

    #[test]
    fn current_tool_unresolved_exec_command() {
        let e = vec![item(
            "function_call",
            json!({"name": "exec_command", "call_id": "c1",
                "arguments": "{\"cmd\":\"cargo  build\",\"workdir\":\"/x\"}"}),
        )];
        let t = extract_current_tool(&e).unwrap();
        assert_eq!(t.name, "exec_command");
        assert_eq!(t.hint.as_deref(), Some("cargo build"));
    }

    #[test]
    fn current_tool_none_when_output_present() {
        let e = vec![
            item(
                "function_call",
                json!({"name": "exec_command", "call_id": "c1",
                    "arguments": "{\"cmd\":\"ls\"}"}),
            ),
            item(
                "function_call_output",
                json!({"call_id": "c1", "output": "ok"}),
            ),
        ];
        assert_eq!(extract_current_tool(&e), None);
    }

    #[test]
    fn current_tool_command_array_hint() {
        let e = vec![item(
            "function_call",
            json!({"name": "shell", "call_id": "c1",
                "arguments": "{\"command\":[\"bash\",\"-lc\",\"ls -la\"]}"}),
        )];
        assert_eq!(
            extract_current_tool(&e).unwrap().hint.as_deref(),
            Some("bash -lc ls -la")
        );
    }

    #[test]
    fn count_tool_uses_counts_function_and_custom_calls() {
        let data = format!(
            "{}\n{}\n{}\n{}\n",
            item(
                "function_call",
                json!({"name": "exec_command", "call_id": "a"})
            ),
            item("message", json!({"role": "assistant", "content": []})),
            item(
                "custom_tool_call",
                json!({"name": "apply_patch", "call_id": "b"})
            ),
            item("function_call_output", json!({"call_id": "a"})),
        );
        assert_eq!(count_tool_uses_in_reader(data.as_bytes()), 2);
    }
}
