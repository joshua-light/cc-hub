//! State-machine explanation: mirrors [`extract_state`](super::extract_state)
//! while recording every rule's inputs and verdict for the debug popup.

use super::state::{
    assistant_asks_question, assistant_awaits_user_input, is_meaningful_entry, QUESTION_TOOLS,
    USER_INPUT_TOOLS,
};
use crate::models::SessionState;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// Rule fired and chose this state. The decision-tree walk stops here.
    Decided(SessionState),
    /// Rule's preconditions matched but it didn't override anything.
    Passed,
    /// Rule's preconditions did not match.
    Skipped,
}

#[derive(Clone, Debug)]
pub struct ExplanationStep {
    pub name: &'static str,
    pub verdict: Verdict,
    pub details: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct EntrySummary {
    pub idx: usize,
    pub kind: String,
    pub timestamp: Option<String>,
    pub stop_reason: Option<String>,
    pub blocks: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct StateExplanation {
    pub final_state: SessionState,
    pub mtime_age_secs: Option<u64>,
    pub entry_count: usize,
    pub steps: Vec<ExplanationStep>,
    pub tail: Vec<EntrySummary>,
}

/// Mirror of `extract_state` (plus the `scanner.rs` Idle→Processing upgrade)
/// that records every rule's inputs and verdict for the debug popup.
pub fn explain_state(entries: &[Value], mtime_age_secs: Option<u64>) -> StateExplanation {
    let mut steps = Vec::new();

    let int_step = explain_interrupted_tool_use(entries);
    let interrupted = matches!(int_step.verdict, Verdict::Decided(_));
    steps.push(int_step);
    if interrupted {
        return finalize(
            SessionState::WaitingForInput,
            mtime_age_secs,
            entries,
            steps,
        );
    }

    let (last_step, base_state) = explain_last_meaningful(entries);
    steps.push(last_step);

    let final_state = if base_state == SessionState::Idle && mtime_age_secs.is_some_and(|s| s < 30)
    {
        steps.push(ExplanationStep {
            name: "mtime_upgrade Idle→Processing",
            verdict: Verdict::Decided(SessionState::Processing),
            details: vec![format!(
                "state is Idle but mtime age {}s < 30s → upgrade to Processing",
                mtime_age_secs.unwrap_or(0)
            )],
        });
        SessionState::Processing
    } else {
        let why = if base_state != SessionState::Idle {
            format!("state is {} (not Idle), no upgrade needed", base_state)
        } else {
            format!(
                "mtime age {} ≥ 30s threshold, no upgrade",
                mtime_age_secs.map_or("unknown".to_string(), |s| format!("{}s", s))
            )
        };
        steps.push(ExplanationStep {
            name: "mtime_upgrade Idle→Processing",
            verdict: Verdict::Skipped,
            details: vec![why],
        });
        base_state
    };

    finalize(final_state, mtime_age_secs, entries, steps)
}

fn finalize(
    final_state: SessionState,
    mtime_age_secs: Option<u64>,
    entries: &[Value],
    steps: Vec<ExplanationStep>,
) -> StateExplanation {
    StateExplanation {
        final_state,
        mtime_age_secs,
        entry_count: entries.len(),
        steps,
        tail: summarize_tail(entries, 12),
    }
}

fn explain_interrupted_tool_use(entries: &[Value]) -> ExplanationStep {
    let mut unresolved: Vec<(usize, String, Option<String>)> = Vec::new();
    let mut results: HashSet<String> = HashSet::new();
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
                    let name = block
                        .get("name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string());
                    unresolved.push((i, id.to_string(), name));
                }
            } else if t == "user" && bt == "tool_result" {
                if let Some(id) = block.get("tool_use_id").and_then(|v| v.as_str()) {
                    results.insert(id.to_string());
                }
            }
        }
    }

    let mut details = vec![
        format!("scanned {} entries", entries.len()),
        format!("found {} assistant tool_use blocks", unresolved.len()),
        format!("found {} user tool_result blocks", results.len()),
        format!(
            "last-prompt entry idx: {}",
            last_prompt_idx.map_or("none".to_string(), |i| i.to_string())
        ),
    ];

    let Some(lp_idx) = last_prompt_idx else {
        details.push("no last-prompt → not interrupted".into());
        return ExplanationStep {
            name: "interrupted_tool_use",
            verdict: Verdict::Skipped,
            details,
        };
    };

    let dangling: Vec<&(usize, String, Option<String>)> = unresolved
        .iter()
        .filter(|(idx, id, _)| *idx < lp_idx && !results.contains(id))
        .collect();

    if dangling.is_empty() {
        details.push(format!(
            "all tool_uses before idx {} have matching tool_results → not interrupted",
            lp_idx
        ));
        ExplanationStep {
            name: "interrupted_tool_use",
            verdict: Verdict::Passed,
            details,
        }
    } else {
        for (idx, id, name) in &dangling {
            details.push(format!(
                "  dangling tool_use idx={} name={} id={} (before last-prompt idx {})",
                idx,
                name.as_deref().unwrap_or("?"),
                short_id(id),
                lp_idx
            ));
        }
        details.push(format!(
            "{} dangling tool_use(s) before last-prompt → INTERRUPTED → WaitingForInput",
            dangling.len()
        ));
        ExplanationStep {
            name: "interrupted_tool_use",
            verdict: Verdict::Decided(SessionState::WaitingForInput),
            details,
        }
    }
}

fn explain_last_meaningful(entries: &[Value]) -> (ExplanationStep, SessionState) {
    let last = entries
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, e)| is_meaningful_entry(e))
        .find(|(_, e)| {
            matches!(
                e.get("type").and_then(|t| t.as_str()),
                Some("user") | Some("assistant")
            )
        });

    let Some((idx, last)) = last else {
        return (
            ExplanationStep {
                name: "last_meaningful_entry",
                verdict: Verdict::Decided(SessionState::Idle),
                details: vec!["no meaningful user/assistant entry → Idle".into()],
            },
            SessionState::Idle,
        );
    };

    let entry_type = last.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let ts = last
        .get("timestamp")
        .and_then(|t| t.as_str())
        .unwrap_or("?");
    let stop = last
        .get("message")
        .and_then(|m| m.get("stop_reason"))
        .and_then(|s| s.as_str())
        .unwrap_or("");

    let mut details = vec![format!(
        "selected: idx={} type={} ts={} stop_reason={:?}",
        idx, entry_type, ts, stop
    )];

    let state = match entry_type {
        "user" => {
            details.push("user message → Processing (assistant about to respond)".into());
            SessionState::Processing
        }
        "assistant" => match stop {
            "end_turn" => {
                details.push("stop_reason=end_turn → WaitingForInput".into());
                SessionState::WaitingForInput
            }
            "tool_use" => {
                let awaits = assistant_awaits_user_input(last);
                let asks_question = awaits && assistant_asks_question(last);
                let tool_names = collect_tool_names(last);
                details.push(format!("tool_use blocks: {:?}", tool_names));
                details.push(format!(
                    "USER_INPUT_TOOLS = {:?} → awaits_user_input={}",
                    USER_INPUT_TOOLS, awaits
                ));
                details.push(format!(
                    "QUESTION_TOOLS = {:?} → asks_question={}",
                    QUESTION_TOOLS, asks_question
                ));
                if asks_question {
                    details.push("AskUserQuestion tool → Question".into());
                    SessionState::Question
                } else if awaits {
                    details.push("blocking tool → WaitingForInput".into());
                    SessionState::WaitingForInput
                } else {
                    details.push("non-blocking tool → Processing".into());
                    SessionState::Processing
                }
            }
            _ => {
                details.push(format!("stop_reason={:?} (other) → Processing", stop));
                SessionState::Processing
            }
        },
        _ => {
            details.push(format!(
                "entry_type={:?} (unexpected) → Processing",
                entry_type
            ));
            SessionState::Processing
        }
    };

    (
        ExplanationStep {
            name: "last_meaningful_entry",
            verdict: Verdict::Decided(state.clone()),
            details,
        },
        state,
    )
}

fn collect_tool_names(entry: &Value) -> Vec<String> {
    let arr = match entry
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        .filter_map(|b| b.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect()
}

fn summarize_tail(entries: &[Value], n: usize) -> Vec<EntrySummary> {
    let start = entries.len().saturating_sub(n);
    entries
        .iter()
        .enumerate()
        .skip(start)
        .map(|(idx, e)| {
            let kind = match e.get("type").and_then(|t| t.as_str()).unwrap_or("?") {
                "system" => format!(
                    "system:{}",
                    e.get("subtype").and_then(|s| s.as_str()).unwrap_or("?")
                ),
                t => t.to_string(),
            };
            let timestamp = e
                .get("timestamp")
                .and_then(|t| t.as_str())
                .map(|s| s.get(11..23).unwrap_or(s).to_string());
            let stop_reason = e
                .get("message")
                .and_then(|m| m.get("stop_reason"))
                .and_then(|s| s.as_str())
                .map(String::from);
            let blocks = e
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|b| {
                            let t = b.get("type").and_then(|t| t.as_str()).unwrap_or("?");
                            match t {
                                "tool_use" => format!(
                                    "tool_use({})",
                                    b.get("name").and_then(|n| n.as_str()).unwrap_or("?")
                                ),
                                "tool_result" => format!(
                                    "tool_result({})",
                                    b.get("tool_use_id")
                                        .and_then(|v| v.as_str())
                                        .map(short_id)
                                        .unwrap_or_else(|| "?".into())
                                ),
                                _ => t.to_string(),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            EntrySummary {
                idx,
                kind,
                timestamp,
                stop_reason,
                blocks,
            }
        })
        .collect()
}

fn short_id(id: &str) -> String {
    let mut s: String = id.chars().take(14).collect();
    if id.chars().count() > 14 {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explain_state_flags_parallel_agent_false_positive() {
        // Reproduces the bug: 3 parallel Agent tool_uses, only 1 resolved,
        // and a `last-prompt` entry sits between the resolved ones — current
        // `interrupted_tool_use` fires and (incorrectly) returns WaitingForInput.
        // The explanation should make the reason explicit.
        let entries = vec![
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-04-16T17:29:42.000Z",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [{"type": "tool_use", "id": "agent_a", "name": "Agent", "input": {}}]}
            }),
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-04-16T17:29:49.000Z",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [{"type": "tool_use", "id": "agent_b", "name": "Agent", "input": {}}]}
            }),
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-04-16T17:29:58.000Z",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [{"type": "tool_use", "id": "agent_c", "name": "Agent", "input": {}}]}
            }),
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-04-16T17:30:25.000Z",
                "message": {"role": "user",
                    "content": [{"type": "tool_result", "tool_use_id": "agent_a"}]}
            }),
            serde_json::json!({"type": "last-prompt", "lastPrompt": "earlier user prompt"}),
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-04-16T17:30:31.000Z",
                "message": {"role": "user",
                    "content": [{"type": "tool_result", "tool_use_id": "agent_b"}]}
            }),
        ];

        let exp = explain_state(&entries, Some(5));
        assert_eq!(exp.final_state, SessionState::WaitingForInput);

        let int_step = exp
            .steps
            .iter()
            .find(|s| s.name == "interrupted_tool_use")
            .expect("interrupted_tool_use step present");
        assert_eq!(
            int_step.verdict,
            Verdict::Decided(SessionState::WaitingForInput),
            "current heuristic still misfires here — that's the bug"
        );
        assert!(
            int_step.details.iter().any(|d| d.contains("agent_c")),
            "explanation should name the dangling tool_use that triggered the verdict"
        );
    }
}
