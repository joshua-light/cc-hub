mod claude;
mod codex;
mod pi;

use super::cost::Tokens;
use crate::agent::AgentKind;
use claude::parse_claude_session_file;
use codex::parse_codex_session_file;
use pi::parse_pi_session_file;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(super) struct ToolUse {
    pub(super) name: String,
    pub(super) id: String,
    /// Parsed argv-0 basenames of every segment of a `Bash` command.
    /// Empty for non-Bash tools.
    pub(super) bash_commands: Vec<String>,
}

/// One canonical assistant API call after dedup.
#[derive(Default)]
pub(super) struct AssistantCall {
    pub(super) model: String,
    pub(super) tokens: Tokens,
    pub(super) timestamp_ms: u64,
    pub(super) tool_uses: Vec<ToolUse>,
    pub(super) cost_override: Option<f64>,
    /// Stable cross-file identity: `requestId`, else `message.id`, else the
    /// per-line uuid. Resume/fork copies history verbatim, so the same call
    /// reappears in a new session's JSONL under the original key; aggregation
    /// dedups on this so a resumed session isn't double-billed. Empty for Pi
    /// sessions, which don't carry these ids.
    pub(super) dedup_key: String,
}

pub(super) struct ParsedSession {
    pub(super) session_id: String,
    pub(super) project: String,
    pub(super) cwd: String,
    pub(super) jsonl_path: PathBuf,
    pub(super) is_subagent: bool,
    pub(super) calls: Vec<AssistantCall>,
    /// All tool_use_ids that received a tool_result (from user messages).
    pub(super) tool_result_ids: HashSet<String>,
    /// tool_use ids issued after the transcript's last genuine user turn (a
    /// `type: "user"` entry carrying non-tool_result content — a new prompt or
    /// the interrupt marker). The conversation never moved on past these, so a
    /// missing result means in-flight or abandoned, not interrupted. Note:
    /// Claude Code writes each content block as its OWN entry, so a parallel
    /// tool batch spans several trailing entries, with non-conversational
    /// chatter (file-history-snapshot, mode, …) freely interleaved — neither
    /// may end the exemption; only a real user turn does.
    pub(super) in_flight_tool_use_ids: HashSet<String>,
}

pub(super) fn parse_session_file(
    path: &Path,
    is_subagent: bool,
    kind: AgentKind,
) -> Option<ParsedSession> {
    match kind {
        AgentKind::Claude => parse_claude_session_file(path, is_subagent),
        AgentKind::Pi => parse_pi_session_file(path),
        AgentKind::Codex => parse_codex_session_file(path),
    }
}

/// A session's project name: the cwd's last component, or `unknown` when the
/// transcript never recorded a cwd.
fn project_of(cwd: Option<&str>) -> String {
    let Some(cwd) = cwd else {
        return "unknown".to_string();
    };
    Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(cwd)
        .to_string()
}

/// Split a Bash invocation into the basenames of its constituent commands.
///
/// Mirrors codeburn's approach: strip quoted strings (so `;` / `|` / `&`
/// inside a literal don't split), tokenize on `;`, `|`, `&`, and take the
/// argv-0 basename of each segment. `cd` and empty segments are dropped.
fn extract_bash_commands(command: &str) -> Vec<String> {
    if command.trim().is_empty() {
        return Vec::new();
    }
    let stripped: String = {
        let mut out = String::with_capacity(command.len());
        let mut quote: Option<char> = None;
        for c in command.chars() {
            match quote {
                Some(q) if c == q => {
                    quote = None;
                    out.push(' ');
                }
                Some(_) => out.push(' '),
                None if c == '"' || c == '\'' => {
                    quote = Some(c);
                    out.push(' ');
                }
                None => out.push(c),
            }
        }
        out
    };
    let mut segments: Vec<&str> = vec![stripped.as_str()];
    for sep in ["&&", "||", ";", "|"] {
        segments = segments.into_iter().flat_map(|s| s.split(sep)).collect();
    }

    let mut cmds = Vec::new();
    for segment in segments {
        let seg = segment.trim();
        if seg.is_empty() {
            continue;
        }
        let first = match seg.split_whitespace().next() {
            Some(t) => t,
            None => continue,
        };
        let base = Path::new(first)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(first);
        if base.is_empty() || base == "cd" {
            continue;
        }
        cmds.push(base.to_string());
    }
    cmds
}
