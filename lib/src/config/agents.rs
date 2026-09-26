//! `[spawn]` and `[agents.<id>]`: which agents exist and how each is launched.

use super::Config;
use crate::agent::{default_claude_models, AgentConfig, AgentKind, AgentModel};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};

impl Config {
    pub fn resolved_agents(&self) -> BTreeMap<String, AgentConfig> {
        let mut out = BTreeMap::new();
        out.insert(
            "claude".into(),
            AgentConfig {
                id: "claude".into(),
                kind: AgentKind::Claude,
                command: self.spawn.command.clone(),
                use_bridge: false,
                models: default_claude_models(),
            },
        );
        for (id, cfg) in &self.agents {
            out.insert(
                id.clone(),
                AgentConfig {
                    id: id.clone(),
                    kind: cfg.kind,
                    command: cfg.command.clone(),
                    use_bridge: cfg.use_bridge,
                    models: if cfg.models.is_empty() && cfg.kind == AgentKind::Claude {
                        default_claude_models()
                    } else {
                        cfg.models.iter().map(ConfiguredModel::resolve).collect()
                    },
                },
            );
        }
        for (id, account) in crate::resources::accounts() {
            out.entry(id.clone()).or_insert_with(|| account.agent(&id));
        }
        out
    }

    pub fn agent(&self, id: &str) -> Option<AgentConfig> {
        self.resolved_agents().remove(id)
    }

    pub fn enabled_agent_kinds(&self) -> HashSet<AgentKind> {
        self.resolved_agents()
            .into_values()
            .map(|a| a.kind)
            .collect()
    }

    pub fn default_session_agent_id(&self) -> String {
        self.projects
            .default_session_agent
            .clone()
            .unwrap_or_else(|| "claude".into())
    }

    /// Sessions-tab hotkeys from `[agents.<id>].hotkey`, keyed by the bound
    /// character. A hotkey must be exactly one character; malformed or
    /// duplicate bindings are dropped with a warning (first agent id in sort
    /// order keeps a contested key) so one typo can't disable the tab.
    fn agent_hotkeys(&self) -> BTreeMap<char, &str> {
        let mut out: BTreeMap<char, &str> = BTreeMap::new();
        for (id, cfg) in &self.agents {
            let Some(raw) = cfg.hotkey.as_deref() else {
                continue;
            };
            let mut chars = raw.chars();
            let key = match (chars.next(), chars.next()) {
                (Some(c), None) if !c.is_whitespace() => c,
                _ => {
                    log::warn!(
                        "config: [agents.{id}].hotkey = {raw:?} is not a single character — ignored"
                    );
                    continue;
                }
            };
            if let Some(prev) = out.get(&key) {
                log::warn!(
                    "config: [agents.{id}].hotkey = {raw:?} already bound to agent {prev} — ignored"
                );
                continue;
            }
            out.insert(key, id.as_str());
        }
        out
    }

    /// Agent id bound to `key` via `[agents.<id>].hotkey`, if any. Borrowed
    /// from the config, so via [`get`](super::get) it is `&'static` and fits in the
    /// `Copy` [`crate::app::SessionsCommand`].
    pub fn agent_for_hotkey(&self, key: char) -> Option<&str> {
        self.agent_hotkeys().remove(&key)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpawnConfig {
    /// The command cc-hub invokes for the default Claude backend. Resolved
    /// through the user's interactive shell so aliases / functions in their rc
    /// file expand — same contract as before config existed.
    pub command: String,
}

impl Default for SpawnConfig {
    fn default() -> Self {
        Self {
            command: "cc-hub-new".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConfiguredAgent {
    pub account: Option<String>,
    pub effort: Option<String>,
    pub kind: AgentKind,
    pub command: String,
    pub use_bridge: bool,
    pub models: Vec<ConfiguredModel>,
    /// Single character that, on the Sessions grid, spawns this agent in the
    /// selected session's cwd — a fixed-agent twin of `n`. Shadows any
    /// built-in Sessions-tab key it collides with.
    pub hotkey: Option<String>,
}

impl Default for ConfiguredAgent {
    fn default() -> Self {
        Self {
            account: None,
            effort: None,
            kind: AgentKind::Claude,
            command: "cc-hub-new".into(),
            use_bridge: false,
            models: Vec::new(),
            hotkey: None,
        }
    }
}

/// A concise model id (`"gpt-5.6"`) or a friendly label/id pair for an
/// entry in the model picker.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ConfiguredModel {
    Id(String),
    Detailed(ConfiguredModelDetails),
}

impl ConfiguredModel {
    fn resolve(&self) -> AgentModel {
        match self {
            Self::Id(id) => AgentModel {
                label: id.clone(),
                id: id.clone(),
            },
            Self::Detailed(model) => AgentModel {
                label: model.label.clone(),
                id: model.id.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredModelDetails {
    pub label: String,
    pub id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_spawn_maps_to_default_claude_agent() {
        let src = r#"
            [spawn]
            command = "my-claude"
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        let agent = cfg.agent("claude").unwrap();
        assert_eq!(agent.kind, AgentKind::Claude);
        assert_eq!(agent.command, "my-claude");
        assert_eq!(agent.models, default_claude_models());
    }

    #[test]
    fn custom_agents_and_defaults_load() {
        let src = r#"
            [agents.pi-codex]
            kind = "pi"
            command = "pi --provider openai-codex"
            use_bridge = true
            models = [
                { label = "GPT-5.6", id = "gpt-5.6" },
                { label = "Sol", id = "sol" },
            ]

            [projects]
            default_orchestrator_agent = "claude"
            default_session_agent = "pi-codex"
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        let pi = cfg.agent("pi-codex").unwrap();
        assert_eq!(pi.kind, AgentKind::Pi);
        assert!(pi.use_bridge);
        assert_eq!(pi.models[0].label, "GPT-5.6");
        assert_eq!(pi.models[0].id, "gpt-5.6");
        assert_eq!(pi.models[1].id, "sol");
        assert_eq!(cfg.default_session_agent_id(), "pi-codex");
    }

    #[test]
    fn agent_hotkeys_map_keys_to_agent_ids() {
        let src = r#"
            [agents.claude]
            hotkey = "N"

            [agents.codex]
            kind = "codex"
            command = "codex --yolo"
            hotkey = "C"

            [agents.pi]
            kind = "pi"
            command = "pi"
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        assert_eq!(cfg.agent_for_hotkey('N'), Some("claude"));
        assert_eq!(cfg.agent_for_hotkey('C'), Some("codex"));
        assert_eq!(cfg.agent_for_hotkey('n'), None);
        assert_eq!(cfg.agent_hotkeys().len(), 2);
    }

    #[test]
    fn agent_hotkeys_drop_malformed_and_duplicate_bindings() {
        let src = r#"
            [agents.a-first]
            hotkey = "x"

            [agents.b-second]
            hotkey = "x"

            [agents.multi]
            hotkey = "xy"

            [agents.blank]
            hotkey = " "
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        let keys = cfg.agent_hotkeys();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[&'x'], "a-first");
    }

    #[test]
    fn agent_models_accept_id_shorthand() {
        let src = r#"
            [agents.pi-codex]
            kind = "pi"
            command = "pi --provider openai-codex"
            models = ["gpt-5.6", "sol"]
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        let models = cfg.agent("pi-codex").unwrap().models;
        assert_eq!(models[0].label, "gpt-5.6");
        assert_eq!(models[0].id, "gpt-5.6");
        assert_eq!(models[1].label, "sol");
        assert_eq!(models[1].id, "sol");
    }
}
