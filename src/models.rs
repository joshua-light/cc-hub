use serde::Deserialize;
use std::fmt;

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
    WaitingForInput,
    Processing,
    ToolExecution,
    Idle,
    Dead,
}

impl SessionState {
    pub fn sort_key(&self) -> u8 {
        match self {
            SessionState::WaitingForInput => 0,
            SessionState::Processing => 1,
            SessionState::ToolExecution => 2,
            SessionState::Idle => 3,
            SessionState::Dead => 4,
        }
    }
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionState::WaitingForInput => write!(f, "waiting for input"),
            SessionState::Processing => write!(f, "processing"),
            SessionState::ToolExecution => write!(f, "tool execution"),
            SessionState::Idle => write!(f, "idle"),
            SessionState::Dead => write!(f, "dead"),
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
    pub alive: bool,
    pub last_user_message: Option<String>,
    pub model: Option<String>,
    pub git_branch: Option<String>,
    pub version: Option<String>,
}

impl SessionInfo {
    pub fn needs_attention(&self) -> bool {
        self.alive && self.state == SessionState::WaitingForInput
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
}

#[derive(Clone, Debug)]
pub struct SessionDetail {
    pub info: SessionInfo,
    pub recent_messages: Vec<ConversationMessage>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
}
