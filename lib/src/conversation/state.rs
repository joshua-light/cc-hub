//! Entry classification and the session-state machine.

use crate::models::SessionState;
use log::debug;
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

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

pub(super) fn is_meaningful_entry(entry: &Value) -> bool {
    // Skip metadata entries (e.g. local-command-caveat after /clear).
    if entry.get("isMeta").and_then(|v| v.as_bool()) == Some(true) {
        return false;
    }

    match entry.get("type").and_then(|t| t.as_str()) {
        Some("user") => {
            if let Some(content) = entry.get("message").and_then(|m| m.get("content")) {
                // Skip tool_result messages (auto-generated, not real user input)
                if let Some(arr) = content.as_array() {
                    let only_tool_results = arr
                        .iter()
                        .all(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"));
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
pub(super) const USER_INPUT_TOOLS: &[&str] = &["EnterPlanMode", "ExitPlanMode", "AskUserQuestion"];

/// Subset of [`USER_INPUT_TOOLS`] that surfaces as the distinct `Question`
/// state instead of generic `WaitingForInput`. Right now it's just
/// `AskUserQuestion` — a structured question vs. plan-mode review needs its
/// own visual treatment so users can tell them apart at a glance.
pub(super) const QUESTION_TOOLS: &[&str] = &["AskUserQuestion"];

/// Returns true if the assistant message contains a tool_use block for a tool
/// that requires user interaction.
pub(super) fn assistant_awaits_user_input(entry: &Value) -> bool {
    assistant_has_blocking_tool(entry, USER_INPUT_TOOLS)
}

/// Returns true if the assistant message contains an unresolved `AskUserQuestion`
/// tool_use — the agent is blocked on a structured question, not just any
/// review/permission prompt.
pub(super) fn assistant_asks_question(entry: &Value) -> bool {
    assistant_has_blocking_tool(entry, QUESTION_TOOLS)
}

fn assistant_has_blocking_tool(entry: &Value, tools: &[&str]) -> bool {
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
                .is_some_and(|name| tools.contains(&name))
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
pub fn extract_state(entries: &[Value]) -> SessionState {
    // Interrupted turn: dangling tool_use + trailing `last-prompt` marker
    // indicates the user hit Esc mid-tool and typed a new message — the
    // session is waiting for them to submit it.
    if interrupted_tool_use(entries) {
        debug!("extract_state: interrupted tool_use (last-prompt follows) → WaitingForInput");
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
                    // QUESTION_TOOLS ⊂ USER_INPUT_TOOLS, so awaits is the
                    // single source of truth for "blocked on user"; the extra
                    // asks_question check just routes the variant.
                    let awaits = assistant_awaits_user_input(last);
                    let asks_question = awaits && assistant_asks_question(last);
                    debug!(
                        "extract_state: last=assistant stop=tool_use asks_question={} awaits_input={}",
                        asks_question, awaits
                    );
                    if asks_question {
                        SessionState::Question
                    } else if awaits {
                        SessionState::WaitingForInput
                    } else {
                        SessionState::Processing
                    }
                }
                _ => {
                    debug!(
                        "extract_state: last=assistant stop_reason={:?} → Processing",
                        stop
                    );
                    SessionState::Processing
                }
            }
        }
        _ => SessionState::Processing,
    };

    debug!("extract_state: last_type={} → {}", entry_type, state);
    state
}

/// Whether the session's most recent assistant entry ends with a `thinking`
/// block — Claude Code writes each content block as its own JSONL entry, so
/// a trailing `thinking` entry with no follow-up `text` or `tool_use` means
/// the agent is still mid-reasoning. Claude Code does not persist the
/// thinking text (only the signature), so we can only report the *fact* that
/// thinking is happening.
pub fn is_currently_thinking(entries: &[Value]) -> bool {
    entries
        .iter()
        .rev()
        .find(|e| e.get("type").and_then(|t| t.as_str()) == Some("assistant"))
        .and_then(|e| {
            let arr = e.get("message")?.get("content")?.as_array()?;
            let last_block = arr.last()?;
            Some(last_block.get("type").and_then(|t| t.as_str()) == Some("thinking"))
        })
        .unwrap_or(false)
}

/// The most recent unresolved assistant `tool_use`: the tool the agent is
/// currently executing (Processing) or the blocking tool it's waiting on
/// user input for (WaitingForInput).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentTool {
    pub name: String,
    /// Short, tool-specific input snippet (Bash command, file basename, grep
    /// pattern, …). None when the tool's input has no useful one-liner.
    pub hint: Option<String>,
}

/// Return the most recent unresolved assistant `tool_use`. Returns None if
/// every `tool_use` has a matching `tool_result`, or if the last meaningful
/// assistant turn had no tool_use.
///
/// We scan the whole window rather than just the last assistant entry so
/// parallel tool calls (multiple tool_use blocks across several assistant
/// entries with results trickling in) resolve to the outstanding one, not
/// an already-completed sibling.
pub fn extract_current_tool(entries: &[Value]) -> Option<CurrentTool> {
    let mut unresolved: Vec<(String, String, Option<String>)> = Vec::new();
    let mut results: HashSet<String> = HashSet::new();

    for entry in entries {
        let t = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");
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
                let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if !id.is_empty() && !name.is_empty() {
                    let hint = format_tool_hint(name, block.get("input"));
                    unresolved.push((id.to_string(), name.to_string(), hint));
                }
            } else if t == "user" && bt == "tool_result" {
                if let Some(id) = block.get("tool_use_id").and_then(|v| v.as_str()) {
                    results.insert(id.to_string());
                }
            }
        }
    }

    unresolved
        .into_iter()
        .rev()
        .find(|(id, _, _)| !results.contains(id))
        .map(|(_, name, hint)| CurrentTool { name, hint })
}

/// One-line input snippet per tool kind. Returns None when the tool's input
/// has no obvious user-facing summary, so the cell renders just the tool name.
fn format_tool_hint(name: &str, input: Option<&Value>) -> Option<String> {
    let input = input?;
    // Strip `mcp__<server>__` prefixes so `mcp__claude_ai_Notion__notion-search`
    // dispatches by its leaf (`notion-search`).
    let leaf = crate::models::mcp_leaf(name);
    let raw = match leaf {
        "Bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        "Edit" | "Read" | "Write" | "NotebookEdit" => {
            input.get("file_path").and_then(|v| v.as_str()).map(|p| {
                Path::new(p)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(p)
                    .to_string()
            })
        }
        "Grep" | "Glob" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        "Task" => input
            .get("description")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        "WebFetch" => input
            .get("url")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        "WebSearch" => input
            .get("query")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        "TodoWrite" => input
            .get("todos")
            .and_then(|v| v.as_array())
            .map(|a| format!("{} todos", a.len())),
        _ => None,
    }?;
    let cleaned = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Sum of input + cache_read + cache_creation tokens from the *most recent*
/// assistant message — i.e. the size of the prompt that gets re-sent on the
/// next turn, which is the live context-window utilisation.
pub fn extract_context_tokens(entries: &[Value]) -> Option<u64> {
    entries
        .iter()
        .rev()
        .find(|e| e.get("type").and_then(|t| t.as_str()) == Some("assistant"))
        .and_then(|e| {
            let usage = e.get("message").and_then(|m| m.get("usage"))?;
            let f = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            let total =
                f("input_tokens") + f("cache_read_input_tokens") + f("cache_creation_input_tokens");
            if total == 0 {
                None
            } else {
                Some(total)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_current_tool_returns_unresolved_among_parallel_calls() {
        // Two parallel Bash + Edit tool_uses, only the Bash one resolved.
        let entries = vec![
            serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [
                        {"type": "tool_use", "id": "t1", "name": "Bash", "input": {}},
                        {"type": "tool_use", "id": "t2", "name": "Edit", "input": {}}
                    ]}
            }),
            serde_json::json!({
                "type": "user",
                "message": {"role": "user",
                    "content": [{"type": "tool_result", "tool_use_id": "t1"}]}
            }),
        ];
        let got = extract_current_tool(&entries).unwrap();
        assert_eq!(got.name, "Edit");
    }

    #[test]
    fn extract_current_tool_none_when_all_resolved() {
        let entries = vec![
            serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "stop_reason": "end_turn",
                    "content": [{"type": "tool_use", "id": "t1", "name": "Read", "input": {}}]}
            }),
            serde_json::json!({
                "type": "user",
                "message": {"role": "user",
                    "content": [{"type": "tool_result", "tool_use_id": "t1"}]}
            }),
        ];
        assert_eq!(extract_current_tool(&entries), None);
    }

    #[test]
    fn extract_current_tool_includes_bash_command_hint() {
        let entries = vec![serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "stop_reason": "tool_use",
                "content": [{"type": "tool_use", "id": "t1", "name": "Bash",
                    "input": {"command": "cargo build --release"}}]}
        })];
        let got = extract_current_tool(&entries).unwrap();
        assert_eq!(got.name, "Bash");
        assert_eq!(got.hint.as_deref(), Some("cargo build --release"));
    }

    #[test]
    fn extract_current_tool_edit_hint_is_basename() {
        let entries = vec![serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "stop_reason": "tool_use",
                "content": [{"type": "tool_use", "id": "t1", "name": "Edit",
                    "input": {"file_path": "/home/u/proj/src/main.rs"}}]}
        })];
        let got = extract_current_tool(&entries).unwrap();
        assert_eq!(got.hint.as_deref(), Some("main.rs"));
    }

    #[test]
    fn extract_current_tool_unknown_tool_has_no_hint() {
        let entries = vec![serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "stop_reason": "tool_use",
                "content": [{"type": "tool_use", "id": "t1", "name": "MysteryTool",
                    "input": {"foo": "bar"}}]}
        })];
        let got = extract_current_tool(&entries).unwrap();
        assert_eq!(got.hint, None);
    }

    #[test]
    fn extract_context_tokens_sums_input_and_cache() {
        let entries = vec![serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "usage": {
                "input_tokens": 1000,
                "cache_read_input_tokens": 50000,
                "cache_creation_input_tokens": 4000,
                "output_tokens": 200
            }}
        })];
        assert_eq!(extract_context_tokens(&entries), Some(55000));
    }

    #[test]
    fn extract_context_tokens_none_when_no_assistant_entry() {
        let entries = vec![serde_json::json!({"type": "user", "message": {"role": "user"}})];
        assert_eq!(extract_context_tokens(&entries), None);
    }

    #[test]
    fn is_currently_thinking_true_when_last_assistant_block_is_thinking() {
        // Claude Code writes each content block as its own JSONL entry — a
        // trailing thinking entry with no text/tool_use follow-up means the
        // agent is still mid-reasoning.
        let entries = vec![
            serde_json::json!({"type": "user", "message": {"role": "user", "content": "hi"}}),
            serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [{"type": "thinking", "thinking": ""}]}
            }),
        ];
        assert!(is_currently_thinking(&entries));
    }

    #[test]
    fn is_currently_thinking_false_when_tool_use_follows() {
        let entries = vec![
            serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [{"type": "thinking", "thinking": ""}]}
            }),
            serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}]}
            }),
        ];
        assert!(!is_currently_thinking(&entries));
    }

    #[test]
    fn is_currently_thinking_false_when_no_assistant_entry() {
        let entries =
            vec![serde_json::json!({"type": "user", "message": {"role": "user", "content": "hi"}})];
        assert!(!is_currently_thinking(&entries));
    }

    #[test]
    fn extract_current_tool_prefers_most_recent_unresolved() {
        // Two assistant turns with unresolved tool_uses; the newer one wins.
        let entries = vec![
            serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [{"type": "tool_use", "id": "old", "name": "Bash", "input": {}}]}
            }),
            serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [{"type": "tool_use", "id": "new", "name": "Grep", "input": {}}]}
            }),
        ];
        let got = extract_current_tool(&entries).unwrap();
        assert_eq!(got.name, "Grep");
    }

    #[test]
    fn extract_state_ask_user_question_yields_question() {
        let entries = vec![serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "stop_reason": "tool_use",
                "content": [{"type": "tool_use", "id": "t1", "name": "AskUserQuestion",
                    "input": {"questions": []}}]}
        })];
        assert_eq!(extract_state(&entries), SessionState::Question);
    }

    #[test]
    fn extract_state_exit_plan_mode_stays_waiting_for_input() {
        // ExitPlanMode is blocking but not a "question" — keep yellow bell.
        let entries = vec![serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "stop_reason": "tool_use",
                "content": [{"type": "tool_use", "id": "t1", "name": "ExitPlanMode",
                    "input": {"plan": "do stuff"}}]}
        })];
        assert_eq!(extract_state(&entries), SessionState::WaitingForInput);
    }
}
