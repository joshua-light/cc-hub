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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// Active API calls — the agent is working.
    Processing,
    /// Has conversation content, waiting for user input.
    WaitingForInput,
    /// Fresh session with no conversation content yet.
    Idle,
    /// Process is gone but the JSONL was modified within the inactive window.
    /// Can be brought back with `cc-hub-new --resume <sid>`.
    Inactive,
}

impl SessionState {
    pub fn sort_key(&self) -> u8 {
        match self {
            SessionState::WaitingForInput => 0,
            SessionState::Idle => 1,
            SessionState::Processing => 2,
            SessionState::Inactive => 3,
        }
    }
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionState::Processing => write!(f, "processing"),
            SessionState::WaitingForInput => write!(f, "waiting for input"),
            SessionState::Idle => write!(f, "idle"),
            SessionState::Inactive => write!(f, "inactive"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub pid: u32,
    pub session_id: String,
    pub cwd: String,
    pub project_name: String,
    pub started_at: u64,
    pub last_activity: Option<u64>,
    pub state: SessionState,
    pub last_user_message: Option<String>,
    pub summary: Option<String>,
    /// 2-3 word Haiku-generated title, cached once per session_id by
    /// [`crate::title`]. `None` until the background titler finishes (or if
    /// it fails — UI falls back to `summary`/`last_user_message`).
    pub title: Option<String>,
    /// True while a background Haiku call is in flight for this session.
    /// Drives the "generating…" indicator in the card so the user can tell
    /// an active titler from a session whose title is simply never going
    /// to arrive (no summary, or resolution failed).
    pub titling: bool,
    pub model: Option<String>,
    pub git_branch: Option<String>,
    pub version: Option<String>,
    pub jsonl_path: Option<PathBuf>,
    /// Name of the tmux session hosting this Claude process, if any. None
    /// means the session is not running under tmux (focus falls back to
    /// walking the pid's own ancestor chain to find a window).
    pub tmux_session: Option<String>,
    /// The most recent unresolved assistant `tool_use` — the tool the agent
    /// is currently running (Processing) or the blocking tool it's waiting on
    /// user input for (WaitingForInput). None if no tool is in flight.
    pub current_tool: Option<crate::conversation::CurrentTool>,
    /// True when the most recent assistant content block is a `thinking`
    /// block (no follow-up tool_use/text has been written yet). Only
    /// meaningful while the session is `Processing`.
    pub is_thinking: bool,
    /// Live context-window utilisation in tokens — the size of the prompt
    /// re-sent on the next turn (input + cache_read + cache_creation from
    /// the most recent assistant message). None if no assistant message has
    /// reported usage yet.
    pub context_tokens: Option<u64>,
}

impl SessionInfo {
    pub fn needs_attention(&self) -> bool {
        self.state == SessionState::WaitingForInput
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
