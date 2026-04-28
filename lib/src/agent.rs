use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Pi,
}

impl AgentKind {
    pub fn badge(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude",
            AgentKind::Pi => "Pi",
        }
    }

    pub fn supports_initial_prompt(self) -> bool {
        matches!(self, AgentKind::Pi)
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.badge())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfig {
    pub id: String,
    pub kind: AgentKind,
    pub command: String,
    pub use_bridge: bool,
}

impl AgentConfig {
    pub fn supports_initial_prompt(&self) -> bool {
        self.kind.supports_initial_prompt()
    }

    pub fn display_label(&self) -> String {
        if self.id == "claude" {
            return "Claude".into();
        }
        if self.id == "pi" {
            return "Pi".into();
        }

        let lower = self.id.to_ascii_lowercase();
        if self.kind == AgentKind::Pi && lower.contains("codex") {
            return "Pi/Codex".into();
        }
        self.id.replace('-', " ")
    }
}
