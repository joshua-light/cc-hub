use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Pi,
    /// OpenAI's `codex` CLI. Like Claude it writes its own rollout transcript
    /// to disk (`~/.codex/sessions/…`), so discovery is transcript-based; but
    /// it has no live status file, so liveness comes from a process scan (see
    /// [`crate::codex_scanner`]).
    Codex,
}

impl AgentKind {
    pub fn badge(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude",
            AgentKind::Pi => "Pi",
            AgentKind::Codex => "Codex",
        }
    }

    pub fn supports_initial_prompt(self) -> bool {
        // Codex takes a positional `[PROMPT]`; Pi takes a trailing prompt arg.
        matches!(self, AgentKind::Pi | AgentKind::Codex)
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
    pub models: Vec<AgentModel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentModel {
    pub label: String,
    pub id: String,
}

pub const DEFAULT_CLAUDE_MODELS: &[(&str, &str)] = &[
    ("Opus 4.8", "claude-opus-4-8"),
    ("Sonnet 5", "claude-sonnet-5"),
    ("Fable 5", "claude-fable-5"),
];

pub fn default_claude_models() -> Vec<AgentModel> {
    DEFAULT_CLAUDE_MODELS
        .iter()
        .map(|(label, id)| AgentModel {
            label: (*label).into(),
            id: (*id).into(),
        })
        .collect()
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
        if self.kind == AgentKind::Codex {
            return if self.id == "codex" {
                "Codex".into()
            } else {
                self.id.replace('-', " ")
            };
        }

        let lower = self.id.to_ascii_lowercase();
        if self.kind == AgentKind::Pi && lower.contains("codex") {
            return "Pi/Codex".into();
        }
        self.id.replace('-', " ")
    }
}
