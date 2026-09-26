use super::{project_name_from_cwd, AssistantCall, ParsedSession, ToolUse};
use crate::conversation::parse_timestamp_ms;
use crate::metrics::cost::Tokens;
use serde_json::Value;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Codex rollouts report token usage as a running `total_token_usage` in
/// repeated `token_count` events, not per-message `usage` blocks — so summing
/// per-turn deltas would multi-count. We take the FINAL cumulative total as a
/// single synthetic call: session/project token and cost totals come out
/// correct, at the cost of per-turn context-growth granularity (codex sessions
/// simply won't surface in the growth findings).
pub(super) fn parse_codex_session_file(path: &Path) -> Option<ParsedSession> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut session_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut cwd: Option<String> = None;
    let mut project: Option<String> = None;
    let mut end_time_ms = 0u64;
    let mut model = String::new();
    let mut total_usage: Option<Value> = None;
    let mut tool_uses: Vec<ToolUse> = Vec::new();
    let mut tool_result_ids: HashSet<String> = HashSet::new();

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v): Result<Value, _> = serde_json::from_str(&line) else {
            continue;
        };
        if let Some(ts) = v.get("timestamp").and_then(parse_timestamp_ms) {
            end_time_ms = end_time_ms.max(ts);
        }
        let rec_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let payload = v.get("payload");
        let payload_type = payload
            .and_then(|p| p.get("type"))
            .and_then(|t| t.as_str())
            .unwrap_or("");
        match (rec_type, payload_type) {
            ("session_meta", _) => {
                if let Some(p) = payload {
                    if let Some(id) = p.get("session_id").and_then(|x| x.as_str()) {
                        session_id = id.to_string();
                    }
                    if let Some(c) = p.get("cwd").and_then(|x| x.as_str()) {
                        cwd = Some(c.to_string());
                        project = Some(project_name_from_cwd(c));
                    }
                }
            }
            ("turn_context", _) => {
                if let Some(m) = payload
                    .and_then(|p| p.get("model"))
                    .and_then(|m| m.as_str())
                {
                    model = m.to_string();
                }
            }
            ("event_msg", "token_count") => {
                if let Some(t) = payload
                    .and_then(|p| p.get("info"))
                    .and_then(|i| i.get("total_token_usage"))
                {
                    total_usage = Some(t.clone());
                }
            }
            ("response_item", "function_call" | "custom_tool_call") => {
                if let Some(p) = payload {
                    let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    if name.is_empty() {
                        continue;
                    }
                    let id = p
                        .get("call_id")
                        .or_else(|| p.get("id"))
                        .and_then(|i| i.as_str())
                        .unwrap_or("");
                    tool_uses.push(ToolUse {
                        name: name.to_string(),
                        id: id.to_string(),
                        bash_commands: Vec::new(),
                    });
                }
            }
            ("response_item", "function_call_output" | "custom_tool_call_output") => {
                if let Some(id) = payload
                    .and_then(|p| p.get("call_id"))
                    .and_then(|i| i.as_str())
                {
                    tool_result_ids.insert(id.to_string());
                }
            }
            _ => {}
        }
    }

    let usage = total_usage?;
    let f = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    let cached = f("cached_input_tokens");
    let tokens = Tokens {
        input: f("input_tokens").saturating_sub(cached),
        // Reasoning is a subset of output_tokens, not additional usage.
        output: f("output_tokens"),
        cache_read: cached,
        cache_creation: 0,
    };
    if tokens.total() == 0 && tool_uses.is_empty() {
        return None;
    }

    let call = AssistantCall {
        model,
        tokens,
        timestamp_ms: end_time_ms,
        tool_uses,
        cost_override: None,
        // Codex resume copies prior history into a new rollout, but we have no
        // per-message id to dedup on; leave empty (same as Pi).
        dedup_key: String::new(),
    };

    Some(ParsedSession {
        session_id,
        project: project.unwrap_or_else(|| "unknown".to_string()),
        cwd: cwd.unwrap_or_default(),
        jsonl_path: path.to_path_buf(),
        is_subagent: false,
        end_time_ms,
        calls: vec![call],
        tool_result_ids,
        in_flight_tool_use_ids: HashSet::new(),
    })
}
