use crate::agent::AgentKind;
use serde::Deserialize;
use std::fmt;
use std::path::PathBuf;

pub fn short_sid(sid: &str) -> &str {
    &sid[..8.min(sid.len())]
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
    pub fn sort_key(&self) -> u8 {
        match self {
            SessionState::Question => 0,
            SessionState::WaitingForInput => 1,
            SessionState::Idle => 2,
            SessionState::Processing => 3,
            SessionState::Inactive => 4,
        }
    }
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionState::Processing => write!(f, "processing"),
            SessionState::WaitingForInput => write!(f, "waiting for input"),
            SessionState::Question => write!(f, "question"),
            SessionState::Idle => write!(f, "idle"),
            SessionState::Inactive => write!(f, "inactive"),
        }
    }
}

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug)]
pub struct ProjectGroup {
    pub name: String,
    pub cwd: String,
    pub sessions: Vec<SessionInfo>,
}
