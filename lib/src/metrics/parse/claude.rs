use super::{extract_bash_commands, project_of, AssistantCall, ParsedSession, ToolUse};
use crate::conversation::parse_timestamp_ms;
use crate::metrics::cost::Tokens;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

pub(super) fn parse_claude_session_file(path: &Path, is_subagent: bool) -> Option<ParsedSession> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    let session_id = path.file_stem()?.to_string_lossy().to_string();
    let mut cwd: Option<String> = None;

    // Dedup: requestId → AssistantCall (latest usage wins).
    let mut by_req: HashMap<String, AssistantCall> = HashMap::new();
    // message.id → canonical requestId for cross-requestId merge.
    let mut msg_id_to_req: HashMap<String, String> = HashMap::new();
    // tool_use_ids that we've already attached to a call — avoids duplicates
    // when the same content block reappears across requestId-redundant lines.
    let mut seen_tool_use_ids: HashSet<String> = HashSet::new();
    // Every tool_use_id that received a tool_result from a user message.
    let mut tool_result_ids: HashSet<String> = HashSet::new();
    // Position of each tool_use in the entry stream, and of the last genuine
    // user turn. An unmatched tool_use only counts as an interruption when a
    // genuine user turn follows it — that's what "the user Esc'd / moved on"
    // actually looks like in the transcript. Everything else (parallel batch
    // entries at the tail, chatter entries between tool_use and result, a
    // hard-killed session) is in-flight/abandoned and must not be charged.
    let mut entry_idx: usize = 0;
    let mut tool_use_pos: HashMap<String, usize> = HashMap::new();
    let mut last_genuine_user_turn: Option<usize> = None;

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
        entry_idx += 1;

        if cwd.is_none() {
            if let Some(c) = v.get("cwd").and_then(|c| c.as_str()) {
                cwd = Some(c.to_string());
            }
        }

        let entry_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

        // User messages: harvest tool_result IDs so we can detect orphans, and
        // track genuine user turns (non-tool_result content: a fresh prompt or
        // the "[Request interrupted by user]" marker; `isMeta` entries are
        // harness-injected, not the human moving on).
        if entry_type == "user" {
            let is_meta = v.get("isMeta").and_then(|m| m.as_bool()).unwrap_or(false);
            let message_content = v.get("message").and_then(|m| m.get("content"));
            let mut genuine =
                matches!(message_content.and_then(|c| c.as_str()), Some(s) if !s.trim().is_empty());
            if let Some(content) = message_content.and_then(|c| c.as_array()) {
                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                        if let Some(id) = block.get("tool_use_id").and_then(|i| i.as_str()) {
                            tool_result_ids.insert(id.to_string());
                        }
                    } else {
                        genuine = true;
                    }
                }
            }
            if genuine && !is_meta {
                last_genuine_user_turn = Some(entry_idx);
            }
            continue;
        }

        if entry_type != "assistant" {
            // Chatter entries (file-history-snapshot, mode, last-prompt, …)
            // interleave freely between a tool_use and its result — they say
            // nothing about whether the conversation moved on.
            continue;
        }

        let request_id = v.get("requestId").and_then(|r| r.as_str()).unwrap_or("");
        let req_id = if request_id.is_empty() {
            v.get("uuid").and_then(|u| u.as_str()).unwrap_or("")
        } else {
            request_id
        };
        if req_id.is_empty() {
            continue;
        }

        let inner = v.get("message");
        let usage = inner.and_then(|m| m.get("usage"));
        let model = inner
            .and_then(|m| m.get("model"))
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let msg_id = inner
            .and_then(|m| m.get("id"))
            .and_then(|m| m.as_str())
            .unwrap_or("");

        // Redirect via message.id if a different requestId already owns
        // this canonical API response.
        let canonical_req = if !msg_id.is_empty() {
            match msg_id_to_req.get(msg_id) {
                Some(existing) if existing != req_id => existing.clone(),
                _ => {
                    msg_id_to_req.insert(msg_id.to_string(), req_id.to_string());
                    req_id.to_string()
                }
            }
        } else {
            req_id.to_string()
        };

        let entry = by_req.entry(canonical_req).or_default();
        if entry.dedup_key.is_empty() {
            // Prefer requestId (survives resume/fork copies), fall back to
            // message.id, then the per-line uuid.
            entry.dedup_key = if !request_id.is_empty() {
                request_id.to_string()
            } else if !msg_id.is_empty() {
                msg_id.to_string()
            } else {
                req_id.to_string()
            };
        }
        if entry.model.is_empty() && !model.is_empty() {
            entry.model = model;
        }
        if let Some(u) = usage {
            // Each line carries cumulative usage for the request — overwrite.
            let tokens = Tokens {
                input: u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                output: u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                cache_read: u
                    .get("cache_read_input_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
                cache_creation: u
                    .get("cache_creation_input_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
            };
            if tokens.total() > 0 {
                entry.tokens = tokens;
            }
        }
        if let Some(ts) = v.get("timestamp").and_then(parse_timestamp_ms) {
            if ts > entry.timestamp_ms {
                entry.timestamp_ms = ts;
            }
        }

        // Tool uses live in message.content[] as blocks with type=tool_use
        // (one block per entry — Claude Code splits parallel batches across
        // consecutive entries).
        if let Some(content) = inner
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        {
            for block in content {
                if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                    continue;
                }
                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("");
                // Record where the tool_use was issued (first sighting wins —
                // requestId-redundant lines repeat blocks) for the in-flight
                // cutoff below, regardless of within-file dedup.
                if !id.is_empty() {
                    tool_use_pos.entry(id.to_string()).or_insert(entry_idx);
                }
                if !id.is_empty() && !seen_tool_use_ids.insert(id.to_string()) {
                    continue;
                }
                let bash_commands = if name == "Bash" {
                    block
                        .get("input")
                        .and_then(|i| i.get("command"))
                        .and_then(|c| c.as_str())
                        .map(extract_bash_commands)
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                entry.tool_uses.push(ToolUse {
                    name: name.to_string(),
                    id: id.to_string(),
                    bash_commands,
                });
            }
        }
    }

    // Exempt every tool_use the conversation never moved past: issued after
    // the last genuine user turn (or in a transcript with none). Their missing
    // results are in-flight/abandoned, not user interruptions.
    let in_flight_tool_use_ids: HashSet<String> = tool_use_pos
        .into_iter()
        .filter(|(_, pos)| last_genuine_user_turn.is_none_or(|u| *pos > u))
        .map(|(id, _)| id)
        .collect();

    let calls: Vec<AssistantCall> = by_req
        .into_values()
        .filter(|c| c.tokens.total() > 0)
        .collect();

    if calls.is_empty() {
        return None;
    }

    Some(ParsedSession {
        session_id,
        project: project_of(cwd.as_deref()),
        cwd: cwd.unwrap_or_default(),
        jsonl_path: path.to_path_buf(),
        is_subagent,
        calls,
        tool_result_ids,
        in_flight_tool_use_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn assistant_tool_use(req: &str, msg: &str, tool_id: &str) -> String {
        format!(
            r#"{{"type":"assistant","requestId":"{req}","message":{{"id":"{msg}","model":"claude-sonnet-5","usage":{{"input_tokens":100,"output_tokens":10}},"content":[{{"type":"tool_use","id":"{tool_id}","name":"Bash","input":{{"command":"ls"}}}}]}}}}"#
        )
    }

    fn parse(lines: &[String]) -> ParsedSession {
        let mut f = tempfile::NamedTempFile::new().expect("tempfile");
        for l in lines {
            writeln!(f, "{}", l).expect("write");
        }
        parse_claude_session_file(f.path(), false).expect("parse")
    }

    #[test]
    fn parallel_batch_at_tail_is_in_flight() {
        // Claude Code splits a parallel batch across consecutive entries; a
        // result for one sibling must not mark the still-running other as an
        // interruption (the tool_result-only user entry is not a genuine turn).
        let s = parse(&[
            assistant_tool_use("r1", "m1", "tu_a"),
            assistant_tool_use("r1", "m1", "tu_b"),
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu_a"}]}}"#.into(),
        ]);
        assert!(s.tool_result_ids.contains("tu_a"));
        assert!(s.in_flight_tool_use_ids.contains("tu_b"));
    }

    #[test]
    fn chatter_after_tool_use_stays_in_flight() {
        // Non-conversational entries interleave between tool_use and result in
        // ~11% of real executions; they must not end the exemption.
        let s = parse(&[
            assistant_tool_use("r1", "m1", "tu_a"),
            r#"{"type":"file-history-snapshot","snapshot":{}}"#.into(),
        ]);
        assert!(s.in_flight_tool_use_ids.contains("tu_a"));
    }

    #[test]
    fn genuine_user_turn_after_tool_use_is_interruption() {
        // A real user turn (text content) after an unmatched tool_use is what
        // an interruption actually looks like — no exemption.
        let s = parse(&[
            assistant_tool_use("r1", "m1", "tu_a"),
            r#"{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#.into(),
        ]);
        assert!(!s.in_flight_tool_use_ids.contains("tu_a"));
        assert!(!s.tool_result_ids.contains("tu_a"));
    }

    #[test]
    fn killed_session_tail_is_not_interruption() {
        // Transcript ends on the tool_use (hard-killed session): nothing moved
        // on, so it's abandoned, not interrupted — deliberately uncounted.
        let s = parse(&[assistant_tool_use("r1", "m1", "tu_a")]);
        assert!(s.in_flight_tool_use_ids.contains("tu_a"));
    }

    #[test]
    fn meta_and_string_content_user_turns() {
        // isMeta user entries are harness-injected — not the human moving on.
        let s = parse(&[
            assistant_tool_use("r1", "m1", "tu_a"),
            r#"{"type":"user","isMeta":true,"message":{"content":"injected caveat"}}"#.into(),
        ]);
        assert!(s.in_flight_tool_use_ids.contains("tu_a"));

        // Plain string content is a genuine prompt.
        let s = parse(&[
            assistant_tool_use("r1", "m1", "tu_a"),
            r#"{"type":"user","message":{"content":"new question"}}"#.into(),
        ]);
        assert!(!s.in_flight_tool_use_ids.contains("tu_a"));
    }
}
