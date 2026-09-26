use super::{extract_bash_commands, project_of, AssistantCall, ParsedSession, ToolUse};
use crate::conversation::parse_timestamp_ms;
use crate::metrics::cost::Tokens;
use serde_json::Value;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

pub(super) fn parse_pi_session_file(path: &Path) -> Option<ParsedSession> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut session_id = path.file_stem()?.to_string_lossy().to_string();
    let mut cwd: Option<String> = None;
    let mut calls: Vec<AssistantCall> = Vec::new();
    let mut tool_result_ids: HashSet<String> = HashSet::new();
    // Tool call ids at the transcript tail — in-flight, not interrupted.
    let mut in_flight_tool_use_ids: HashSet<String> = HashSet::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "session" => {
                if let Some(id) = v.get("id").and_then(|x| x.as_str()) {
                    session_id = id.to_string();
                }
                if let Some(c) = v.get("cwd").and_then(|x| x.as_str()) {
                    cwd = Some(c.to_string());
                }
            }
            "message" => {
                let Some(msg) = v.get("message") else {
                    continue;
                };
                match msg.get("role").and_then(|r| r.as_str()) {
                    Some("assistant") => {
                        let usage = msg.get("usage");
                        let f = |k: &str| {
                            usage
                                .and_then(|u| u.get(k))
                                .and_then(|x| x.as_u64())
                                .unwrap_or(0)
                        };
                        let tokens = Tokens {
                            input: f("input"),
                            output: f("output"),
                            cache_read: f("cacheRead"),
                            cache_creation: f("cacheWrite"),
                        };
                        let cost_override = usage
                            .and_then(|u| u.get("cost"))
                            .and_then(|c| c.get("total"))
                            .and_then(|v| v.as_f64());
                        let mut call = AssistantCall {
                            model: msg
                                .get("model")
                                .and_then(|m| m.as_str())
                                .unwrap_or("")
                                .to_string(),
                            tokens,
                            timestamp_ms: v
                                .get("timestamp")
                                .and_then(parse_timestamp_ms)
                                .unwrap_or(0),
                            tool_uses: Vec::new(),
                            cost_override,
                            // Pi sessions carry no requestId/message.id; cross-file
                            // dedup (BUG 5) doesn't apply, so leave the key empty.
                            dedup_key: String::new(),
                        };
                        if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                            for block in content {
                                if block.get("type").and_then(|t| t.as_str()) != Some("toolCall") {
                                    continue;
                                }
                                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                                if name.is_empty() {
                                    continue;
                                }
                                let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("");
                                let bash_commands = if name == "bash" {
                                    block
                                        .get("arguments")
                                        .and_then(|i| i.get("command"))
                                        .and_then(|c| c.as_str())
                                        .map(extract_bash_commands)
                                        .unwrap_or_default()
                                } else {
                                    Vec::new()
                                };
                                call.tool_uses.push(ToolUse {
                                    name: name.to_string(),
                                    id: id.to_string(),
                                    bash_commands,
                                });
                            }
                        }
                        // A trailing assistant toolCall with no result yet is
                        // in-flight; any later entry clears this.
                        in_flight_tool_use_ids = call
                            .tool_uses
                            .iter()
                            .map(|t| t.id.clone())
                            .filter(|id| !id.is_empty())
                            .collect();
                        if call.tokens.total() > 0 || !call.tool_uses.is_empty() {
                            calls.push(call);
                        }
                    }
                    Some("toolResult") => {
                        if let Some(id) = msg.get("toolCallId").and_then(|i| i.as_str()) {
                            tool_result_ids.insert(id.to_string());
                        }
                        in_flight_tool_use_ids.clear();
                    }
                    _ => {
                        in_flight_tool_use_ids.clear();
                    }
                }
            }
            _ => {}
        }
    }

    if calls.is_empty() {
        return None;
    }

    Some(ParsedSession {
        session_id,
        project: project_of(cwd.as_deref()),
        cwd: cwd.unwrap_or_default(),
        jsonl_path: path.to_path_buf(),
        is_subagent: false,
        calls,
        tool_result_ids,
        in_flight_tool_use_ids,
    })
}
