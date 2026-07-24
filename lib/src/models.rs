use crate::agent::AgentKind;
use serde::Deserialize;
use std::fmt;
use std::path::PathBuf;

pub fn short_sid(sid: &str) -> &str {
    &sid[..8.min(sid.len())]
}

/// Canonical relative-age formatting. `secs` is the elapsed time in seconds.
/// Renders the largest whole unit (`s`/`m`/`h`/`d`) with an ` ago` suffix —
/// e.g. `45s ago`, `3m ago`, `2h ago`, `5d ago`. Used wherever cc-hub shows
/// "how long ago" a task/session/banner was updated.
pub fn relative_age(secs: u64) -> String {
    format!("{} ago", relative_age_short(secs))
}

/// Like [`relative_age`] but without the ` ago` suffix — `45s`, `3m`, `2h`,
/// `5d`. For tight columns where the suffix doesn't fit.
pub fn relative_age_short(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// Canonical first-line preview: take the first line of `text` and, if it
/// exceeds `max` characters, truncate it to a `max`-char budget ending in a
/// single-char ellipsis (`…`). The result is always at most `max` characters
/// wide (counting the ellipsis), so it fits a `max`-column slot. Replaces the
/// several drifted `one_line`/`first_line_preview`/`truncate_str` copies that
/// disagreed on the ellipsis string and the boundary math.
pub fn first_line_truncated(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or(text);
    if line.chars().count() <= max {
        return line.to_string();
    }
    if max == 0 {
        return String::new();
    }
    if max == 1 {
        return "…".to_string();
    }
    let mut out: String = line.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// Leaf of an MCP tool name: `mcp__<server>__<tool>` → `<tool>` (the
/// distinctive part). Plain tool names like `Bash` pass through unchanged.
pub fn mcp_leaf(name: &str) -> &str {
    name.rsplit("__").next().unwrap_or(name)
}

/// Server segment of an MCP tool name: `mcp__<server>__<tool>` → `<server>`.
/// Returns `None` for non-MCP tool names (those without the `mcp__` prefix).
pub fn mcp_server(name: &str) -> Option<&str> {
    name.strip_prefix("mcp__")
        .map(|rest| rest.split("__").next().unwrap_or(rest))
}

#[derive(Deserialize)]
pub struct RawSession {
    pub pid: u32,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub cwd: String,
    #[serde(rename = "startedAt")]
    pub started_at: u64,
    /// Live status Claude Code writes to the session file: `"busy"`,
    /// `"idle"`, or `"waiting"`. `"waiting"` means the agent is blocked on an
    /// interactive prompt (AskUserQuestion, permission prompt, or plan
    /// review) — and crucially, it is set the moment the prompt opens, before
    /// the prompt's tool_use is flushed to the transcript. Absent on older
    /// clients that don't write it.
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// App-synthesized only: a spawn-watch placeholder for an agent that was
    /// just launched but hasn't registered a session file yet. The scanner
    /// never emits this state.
    Starting,
    Processing,
    WaitingForInput,
    /// Agent's latest unresolved tool_use is `AskUserQuestion` — it is
    /// specifically waiting on a structured answer, not just "any input".
    /// Treated like WaitingForInput everywhere except in the UI, where it
    /// gets a distinct blue question-mark indicator.
    Question,
    Idle,
    Inactive,
}

impl SessionState {
    /// Coarse liveness bucket for ordering: all active flavors rank equally
    /// (they flip between each other every few seconds, and ordering on that
    /// churn made cards swap under the cursor), idle sorts after them,
    /// inactive last. Starting ranks with Idle deliberately: a fresh spawn
    /// first appears in a scan as Idle, so its placeholder must occupy the
    /// slot the real card will land in — ranking it "active" made the card
    /// jump from the top of the group to the idle band once the scanner took
    /// over.
    pub fn liveness_rank(&self) -> u8 {
        match self {
            SessionState::Processing | SessionState::WaitingForInput | SessionState::Question => 0,
            SessionState::Starting | SessionState::Idle => 1,
            SessionState::Inactive => 2,
        }
    }
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionState::Starting => write!(f, "starting"),
            SessionState::Processing => write!(f, "processing"),
            SessionState::WaitingForInput => write!(f, "waiting for input"),
            SessionState::Question => write!(f, "question"),
            SessionState::Idle => write!(f, "idle"),
            SessionState::Inactive => write!(f, "inactive"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionInfo {
    pub agent_id: String,
    pub agent_kind: AgentKind,
    pub pid: u32,
    pub session_id: String,
    pub cwd: String,
    pub project_name: String,
    pub started_at: u64,
    pub last_activity: Option<u64>,
    pub state: SessionState,
    pub last_user_message: Option<String>,
    pub summary: Option<String>,
    pub title: Option<String>,
    pub titling: bool,
    pub model: Option<String>,
    pub git_branch: Option<String>,
    pub version: Option<String>,
    pub jsonl_path: Option<PathBuf>,
    pub tmux_session: Option<String>,
    pub current_tool: Option<crate::conversation::CurrentTool>,
    pub is_thinking: bool,
    pub context_tokens: Option<u64>,
    pub tool_uses_count: u64,
}

impl SessionInfo {
    pub fn needs_attention(&self) -> bool {
        matches!(
            self.state,
            SessionState::WaitingForInput | SessionState::Question
        )
    }

    pub fn agent_badge(&self) -> String {
        if self.agent_id == "claude" {
            return "Claude".into();
        }
        if self.agent_id == "pi" {
            return "Pi".into();
        }
        if self.agent_kind == AgentKind::Codex {
            return if self.agent_id == "codex" {
                "Codex".into()
            } else {
                self.agent_id.replace('-', " ")
            };
        }
        let lower = self.agent_id.to_ascii_lowercase();
        if self.agent_kind == AgentKind::Pi && lower.contains("codex") {
            return "Pi/Codex".into();
        }
        self.agent_id.replace('-', " ")
    }
}

#[derive(Clone, Debug)]
pub struct ConversationMessage {
    pub role: String,
    pub content_preview: String,
    pub timestamp: u64,
    pub model: Option<String>,
    pub stop_reason: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct SessionDetail {
    pub info: SessionInfo,
    pub recent_messages: Vec<ConversationMessage>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
}

/// Badge for a session card linked (`L`) to a task: the task title rendered
/// on the card in a color derived from `task_id`, so every card of the same
/// task carries the same mark. `stale` marks a task that is Done or no
/// longer readable — the badge renders dimmed so the link visibly outlived
/// its task.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskBadge {
    pub task_id: String,
    pub title: String,
    /// Priority of the linked task while it's still readable; `None` once
    /// only the sidecar title snapshot remains (it records no priority).
    pub priority: Option<crate::orchestrator::TaskPriority>,
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectGroup {
    pub name: String,
    pub cwd: String,
    pub sessions: Vec<SessionInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_age_units() {
        assert_eq!(relative_age(0), "0s ago");
        assert_eq!(relative_age(45), "45s ago");
        assert_eq!(relative_age(60), "1m ago");
        assert_eq!(relative_age(3599), "59m ago");
        assert_eq!(relative_age(3600), "1h ago");
        assert_eq!(relative_age(86_399), "23h ago");
        assert_eq!(relative_age(86_400), "1d ago");
        assert_eq!(relative_age_short(90), "1m");
        assert_eq!(relative_age_short(7200), "2h");
    }

    #[test]
    fn first_line_truncated_takes_only_first_line() {
        assert_eq!(first_line_truncated("hello\nworld", 80), "hello");
        assert_eq!(first_line_truncated("", 10), "");
    }

    #[test]
    fn first_line_truncated_char_budget() {
        // Fits exactly — no ellipsis.
        assert_eq!(first_line_truncated("abcde", 5), "abcde");
        // Over budget — total chars equals `max`, single ellipsis.
        let out = first_line_truncated("abcdef", 5);
        assert_eq!(out, "abcd…");
        assert_eq!(out.chars().count(), 5);
    }

    #[test]
    fn first_line_truncated_multibyte_safe() {
        // Four 2-byte chars; budget 3 → 2 chars + ellipsis, on char boundaries.
        let out = first_line_truncated("αααα", 3);
        assert_eq!(out, "αα…");
        assert_eq!(out.chars().count(), 3);
    }

    #[test]
    fn first_line_truncated_tiny_budgets() {
        assert_eq!(first_line_truncated("abcdef", 0), "");
        assert_eq!(first_line_truncated("abcdef", 1), "…");
    }

    #[test]
    fn mcp_leaf_and_server() {
        assert_eq!(
            mcp_leaf("mcp__claude_ai_Notion__notion-search"),
            "notion-search"
        );
        assert_eq!(mcp_leaf("Bash"), "Bash");
        assert_eq!(
            mcp_server("mcp__claude_ai_Notion__notion-search"),
            Some("claude_ai_Notion")
        );
        assert_eq!(mcp_server("Bash"), None);
        // Malformed: prefix only, no second `__`.
        assert_eq!(mcp_server("mcp__solo"), Some("solo"));
    }
}
