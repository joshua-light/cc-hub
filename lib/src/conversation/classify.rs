//! The one shared session-state decision procedure.
//!
//! Each transcript backend (Claude JSONL, Pi JSONL) answers *format*
//! questions through [`TranscriptDialect`]; the *meaning* of those answers —
//! which stop reasons end a turn, when a blocking tool surfaces as
//! `Question` vs `WaitingForInput` — lives here, once. A fix to the state
//! machine applies to every backend, and a backend that drifts fails the
//! parity matrix below instead of silently misclassifying in production.

use crate::models::SessionState;
use log::debug;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Meaningful conversational role of an entry. `None` marks entries the
/// state machine must skip (metadata, tool_result-only user lines, local
/// commands, system lines).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    User,
    Assistant,
}

/// What a backend's stop-reason string means to the shared state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StopMapping {
    /// The turn completed; the agent waits for the user.
    EndOfTurn,
    /// The agent requested a tool call; blocking-tool inspection decides.
    ToolUse,
    /// A stop reason this dialect doesn't recognize. Logged once per
    /// (dialect, path, reason) and treated as Processing — a new stop
    /// reason shipped by an agent update degrades visibly, not silently.
    Unknown,
}

/// Whether an assistant tool_use blocks on the user, and how it surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockingTool {
    None,
    WaitsForInput,
    Question,
}

/// Format adapter for one transcript dialect. Implementations answer syntax
/// questions only; every semantic decision belongs in [`classify`].
pub(crate) trait TranscriptDialect {
    /// Dialect tag for log lines ("claude", "pi").
    const NAME: &'static str;

    /// Conversational role of `entry`, or `None` for entries the state
    /// machine must skip.
    fn role(&self, entry: &Value) -> Option<Role>;

    /// Raw stop-reason string of an assistant entry, if present. A missing
    /// or empty stop reason means the entry is still being streamed —
    /// [`classify`] treats that as Processing without consulting
    /// [`Self::map_stop`], so in-flight turns never trip the unknown warn.
    fn stop_reason<'a>(&self, entry: &'a Value) -> Option<&'a str>;

    /// Map a non-empty stop-reason string onto the shared semantics.
    fn map_stop(&self, stop: &str) -> StopMapping;

    /// Whether `entry` is the dialect's synthetic user-interrupt marker.
    fn is_interrupt_marker(&self, entry: &Value) -> bool;

    /// Whether an assistant entry's tool_use blocks on user interaction.
    fn blocking_tool(&self, entry: &Value) -> BlockingTool;
}

/// Derive the session state from a transcript tail. `source` is the JSONL
/// path, used only to key and label the once-per-path unknown-stop warning.
pub(crate) fn classify<D: TranscriptDialect>(
    dialect: &D,
    entries: &[Value],
    source: Option<&Path>,
) -> SessionState {
    let last = entries
        .iter()
        .rev()
        .find_map(|e| dialect.role(e).map(|role| (role, e)));
    let Some((role, entry)) = last else {
        debug!("classify[{}]: no meaningful entry → Idle", D::NAME);
        return SessionState::Idle;
    };
    let state = match role {
        Role::User if dialect.is_interrupt_marker(entry) => SessionState::WaitingForInput,
        Role::User => SessionState::Processing,
        Role::Assistant => match dialect.stop_reason(entry) {
            // In-flight: Claude Code streams assistant entries with a null
            // stop_reason until the turn settles. Normal, never "unknown".
            None | Some("") => SessionState::Processing,
            Some(stop) => match dialect.map_stop(stop) {
                StopMapping::EndOfTurn => SessionState::WaitingForInput,
                StopMapping::ToolUse => match dialect.blocking_tool(entry) {
                    BlockingTool::Question => SessionState::Question,
                    BlockingTool::WaitsForInput => SessionState::WaitingForInput,
                    BlockingTool::None => SessionState::Processing,
                },
                StopMapping::Unknown => {
                    if note_unknown_stop(D::NAME, source, stop) {
                        log::warn!(
                            "classify[{}]: unknown stop_reason {:?} in {} → Processing",
                            D::NAME,
                            stop,
                            source
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| "<unknown>".into()),
                        );
                    }
                    SessionState::Processing
                }
            },
        },
    };
    debug!("classify[{}]: last={:?} → {}", D::NAME, role, state);
    state
}

/// Record an unknown stop reason; returns true only on first sighting of
/// this (dialect, path, reason) triple so the warn fires once per process
/// instead of on every 2s scan tick.
fn note_unknown_stop(dialect: &str, source: Option<&Path>, stop: &str) -> bool {
    static SEEN: OnceLock<Mutex<HashSet<(String, PathBuf, String)>>> = OnceLock::new();
    let key = (
        dialect.to_string(),
        source.map(Path::to_path_buf).unwrap_or_default(),
        stop.to_string(),
    );
    let mut seen = SEEN
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    seen.insert(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::state::ClaudeDialect;
    use crate::pi_conversation::PiDialect;
    use serde_json::json;

    // --- synthetic entry builders, one set per dialect -------------------

    fn c_user(text: &str) -> Value {
        json!({"type": "user", "message": {"role": "user", "content": text}})
    }
    fn c_assistant(stop: Option<&str>, content: Value) -> Value {
        let mut msg = json!({"role": "assistant", "content": content});
        if let Some(s) = stop {
            msg["stop_reason"] = json!(s);
        }
        json!({"type": "assistant", "message": msg})
    }
    fn c_tool_use(name: &str) -> Value {
        json!([{"type": "tool_use", "id": "tu_1", "name": name, "input": {}}])
    }
    fn c_tool_result_only() -> Value {
        json!({"type": "user", "message": {"role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "ok"}]}})
    }
    fn c_interrupt() -> Value {
        c_user("[Request interrupted by user]")
    }

    fn p_user(text: &str) -> Value {
        json!({"type": "message", "message": {"role": "user", "content": text}})
    }
    fn p_assistant(stop: Option<&str>, content: Value) -> Value {
        let mut msg = json!({"role": "assistant", "content": content});
        if let Some(s) = stop {
            msg["stopReason"] = json!(s);
        }
        json!({"type": "message", "message": msg})
    }
    fn p_tool_call(name: &str) -> Value {
        json!([{"type": "toolCall", "id": "tc_1", "name": name, "arguments": {}}])
    }

    struct Case {
        name: &'static str,
        claude: Vec<Value>,
        expect_claude: SessionState,
        pi: Vec<Value>,
        expect_pi: SessionState,
    }

    /// The parity matrix: the same conversational situation expressed in
    /// each dialect's JSON shape, with the state each must derive. Rows
    /// where the dialects legitimately diverge (Pi has no blocking tools,
    /// no interrupt marker, and maps error/aborted to end-of-turn) encode
    /// that divergence explicitly so accidental drift fails loudly.
    fn matrix() -> Vec<Case> {
        use SessionState::*;
        vec![
            Case {
                name: "empty transcript",
                claude: vec![],
                expect_claude: Idle,
                pi: vec![],
                expect_pi: Idle,
            },
            Case {
                name: "last = user prompt",
                claude: vec![c_user("do the thing")],
                expect_claude: Processing,
                pi: vec![p_user("do the thing")],
                expect_pi: Processing,
            },
            Case {
                name: "turn complete (end_turn / stop)",
                claude: vec![
                    c_user("hi"),
                    c_assistant(Some("end_turn"), json!([{"type":"text","text":"done"}])),
                ],
                expect_claude: WaitingForInput,
                pi: vec![
                    p_user("hi"),
                    p_assistant(Some("stop"), json!([{"type":"text","text":"done"}])),
                ],
                expect_pi: WaitingForInput,
            },
            Case {
                name: "non-blocking tool call",
                claude: vec![c_assistant(Some("tool_use"), c_tool_use("Bash"))],
                expect_claude: Processing,
                pi: vec![p_assistant(Some("toolUse"), p_tool_call("bash"))],
                expect_pi: Processing,
            },
            Case {
                name: "blocking tool (plan review)",
                claude: vec![c_assistant(Some("tool_use"), c_tool_use("ExitPlanMode"))],
                expect_claude: WaitingForInput,
                // Pi has no blocking tools: the same shape stays Processing.
                pi: vec![p_assistant(Some("toolUse"), p_tool_call("ExitPlanMode"))],
                expect_pi: Processing,
            },
            Case {
                name: "structured question",
                claude: vec![c_assistant(Some("tool_use"), c_tool_use("AskUserQuestion"))],
                expect_claude: Question,
                pi: vec![p_assistant(Some("toolUse"), p_tool_call("AskUserQuestion"))],
                expect_pi: Processing,
            },
            Case {
                name: "trailing interrupt marker",
                claude: vec![
                    c_assistant(Some("tool_use"), c_tool_use("Bash")),
                    c_interrupt(),
                ],
                expect_claude: WaitingForInput,
                // Pi has no interrupt marker; plain trailing user text.
                pi: vec![
                    p_assistant(Some("toolUse"), p_tool_call("bash")),
                    p_user("[Request interrupted by user]"),
                ],
                expect_pi: Processing,
            },
            Case {
                name: "error / aborted stop",
                // Claude has no error stop reason today: Unknown → Processing.
                claude: vec![c_assistant(Some("error"), json!([]))],
                expect_claude: Processing,
                // Pi maps error to end-of-turn: the agent stopped.
                pi: vec![p_assistant(Some("error"), json!([]))],
                expect_pi: WaitingForInput,
            },
            Case {
                name: "unknown stop reason",
                claude: vec![c_assistant(Some("banana"), json!([]))],
                expect_claude: Processing,
                pi: vec![p_assistant(Some("banana"), json!([]))],
                expect_pi: Processing,
            },
            Case {
                name: "in-flight entry (no stop reason)",
                claude: vec![c_assistant(None, json!([{"type":"text","text":"…"}]))],
                expect_claude: Processing,
                pi: vec![p_assistant(None, json!([{"type":"text","text":"…"}]))],
                expect_pi: Processing,
            },
            Case {
                name: "trailing tool_result-only user entry is skipped",
                claude: vec![
                    c_assistant(Some("tool_use"), c_tool_use("Bash")),
                    c_tool_result_only(),
                ],
                // Judged on the prior assistant entry: still running the tool.
                expect_claude: Processing,
                // Pi tool results are their own role; role() skips them.
                pi: vec![
                    p_assistant(Some("toolUse"), p_tool_call("bash")),
                    json!({"type": "message", "message": {"role": "toolResult", "toolCallId": "tc_1"}}),
                ],
                expect_pi: Processing,
            },
        ]
    }

    #[test]
    fn parity_matrix() {
        for case in matrix() {
            assert_eq!(
                classify(&ClaudeDialect, &case.claude, None),
                case.expect_claude,
                "claude: {}",
                case.name
            );
            assert_eq!(
                classify(&PiDialect, &case.pi, None),
                case.expect_pi,
                "pi: {}",
                case.name
            );
        }
    }

    #[test]
    fn unknown_stop_notes_once_per_triple() {
        let p = Path::new("/tmp/classify-test-unique-a.jsonl");
        assert!(note_unknown_stop("claude", Some(p), "warp_drive"));
        assert!(!note_unknown_stop("claude", Some(p), "warp_drive"));
        // Different path or reason is a fresh sighting.
        let q = Path::new("/tmp/classify-test-unique-b.jsonl");
        assert!(note_unknown_stop("claude", Some(q), "warp_drive"));
        assert!(note_unknown_stop("claude", Some(p), "warp_drive_2"));
        assert!(note_unknown_stop("pi", Some(p), "warp_drive"));
    }
}
