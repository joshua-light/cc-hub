//! User config at `~/.cc-hub/config.toml`, loaded once and exposed via
//! [`get`]. Missing file, missing section, and missing field all fall back
//! to [`Default`], so this is a pure knob layer — removing the file yields
//! the same behaviour as shipped defaults.

use crate::agent::{AgentConfig, AgentKind};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

pub fn config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cc-hub").join("config.toml"))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub spawn: SpawnConfig,
    pub agents: BTreeMap<String, ConfiguredAgent>,
    pub projects: ProjectsConfig,
    pub title: TitleConfig,
    pub inactive: InactiveConfig,
    pub scan: ScanConfig,
    pub ui: UiConfig,
    pub metrics: MetricsConfig,
    pub backlog: BacklogConfig,
}

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
                },
            );
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

    pub fn default_orchestrator_agent_id(&self) -> String {
        self.projects
            .default_orchestrator_agent
            .clone()
            .unwrap_or_else(|| "claude".into())
    }

    pub fn default_worker_agent_id(&self) -> String {
        self.projects
            .default_worker_agent
            .clone()
            .unwrap_or_else(|| "claude".into())
    }

    pub fn default_session_agent_id(&self) -> String {
        self.projects
            .default_session_agent
            .clone()
            .unwrap_or_else(|| "claude".into())
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
    pub kind: AgentKind,
    pub command: String,
    pub use_bridge: bool,
}

impl Default for ConfiguredAgent {
    fn default() -> Self {
        Self {
            kind: AgentKind::Claude,
            command: "cc-hub-new".into(),
            use_bridge: false,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectsConfig {
    pub default_orchestrator_agent: Option<String>,
    pub default_worker_agent: Option<String>,
    pub default_session_agent: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TitleConfig {
    pub enabled: bool,
    pub model: String,
    pub max_length: usize,
    pub run_timeout_secs: u64,
    pub resolve_timeout_secs: u64,
    pub concurrency: usize,
    pub prompt: String,
}

impl TitleConfig {
    pub fn run_timeout(&self) -> Duration {
        Duration::from_secs(self.run_timeout_secs)
    }
    pub fn resolve_timeout(&self) -> Duration {
        Duration::from_secs(self.resolve_timeout_secs)
    }
}

const DEFAULT_TITLE_PROMPT: &str =
    "Output a 2 or 3 word title summarizing this coding-agent user request. \
     Output only the title — no quotes, no punctuation, no prefix like \
     \"Title:\". Just the words.\n\nRequest:\n";

impl Default for TitleConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "haiku".into(),
            max_length: 40,
            run_timeout_secs: 45,
            resolve_timeout_secs: 10,
            concurrency: 2,
            prompt: DEFAULT_TITLE_PROMPT.into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InactiveConfig {
    pub window_secs: u64,
    pub max_per_project: usize,
}

impl Default for InactiveConfig {
    fn default() -> Self {
        Self {
            window_secs: 3 * 86_400,
            max_per_project: 5,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScanConfig {
    pub fs_fallback_interval_secs: u64,
    pub usage_refresh_interval_secs: u64,
    pub usage_cache_ttl_secs: u64,
}

impl ScanConfig {
    pub fn fs_fallback_interval(&self) -> Duration {
        Duration::from_secs(self.fs_fallback_interval_secs)
    }
    pub fn usage_refresh_interval(&self) -> Duration {
        Duration::from_secs(self.usage_refresh_interval_secs)
    }
    pub fn usage_cache_ttl(&self) -> Duration {
        Duration::from_secs(self.usage_cache_ttl_secs)
    }
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            fs_fallback_interval_secs: 2,
            usage_refresh_interval_secs: 60,
            usage_cache_ttl_secs: 60,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    pub status_msg_ttl_secs: u64,
    pub pending_dispatch_timeout_secs: u64,
    pub cell_height: u16,
    pub cell_width: u16,
}

impl UiConfig {
    pub fn status_msg_ttl(&self) -> Duration {
        Duration::from_secs(self.status_msg_ttl_secs)
    }
    pub fn pending_dispatch_timeout(&self) -> Duration {
        Duration::from_secs(self.pending_dispatch_timeout_secs)
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            status_msg_ttl_secs: 5,
            pending_dispatch_timeout_secs: 60,
            cell_height: 7,
            cell_width: 42,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MetricsConfig {
    pub min_growth_turns: usize,
    pub growth_threshold: f64,
    pub top_interruptions: usize,
    pub top_growth_findings: usize,
    pub top_peak_context_findings: usize,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            min_growth_turns: 20,
            growth_threshold: 6.0,
            top_interruptions: 10,
            top_growth_findings: 10,
            top_peak_context_findings: 10,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BacklogConfig {
    pub enabled: bool,
    pub model: String,
    pub interval_secs: u64,
    pub run_timeout_secs: u64,
    pub ttl_secs: u64,
}

impl BacklogConfig {
    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs)
    }
    pub fn run_timeout(&self) -> Duration {
        Duration::from_secs(self.run_timeout_secs)
    }
}

impl Default for BacklogConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: "sonnet".into(),
            interval_secs: 8,
            run_timeout_secs: 120,
            ttl_secs: 300,
        }
    }
}

pub fn get() -> &'static Config {
    static CFG: OnceLock<Config> = OnceLock::new();
    CFG.get_or_init(load)
}

fn load() -> Config {
    let Some(path) = config_path() else {
        log::debug!("config: no home dir, using defaults");
        return Config::default();
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::debug!("config: {} not found, using defaults", path.display());
            return Config::default();
        }
        Err(e) => {
            log::warn!(
                "config: read error at {}: {} — using defaults",
                path.display(),
                e
            );
            return Config::default();
        }
    };
    match toml::from_str::<Config>(&raw) {
        Ok(cfg) => {
            log::info!("config: loaded {}", path.display());
            cfg
        }
        Err(e) => {
            log::warn!(
                "config: parse error in {}: {} — using defaults",
                path.display(),
                e
            );
            Config::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_toml_yields_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        let def = Config::default();
        assert_eq!(cfg.spawn.command, def.spawn.command);
        assert_eq!(cfg.title.model, def.title.model);
        assert_eq!(cfg.inactive.window_secs, def.inactive.window_secs);
        assert_eq!(cfg.default_orchestrator_agent_id(), "claude");
    }

    #[test]
    fn partial_section_merges_with_defaults() {
        let src = r#"
            [title]
            model = "sonnet"
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        assert_eq!(cfg.title.model, "sonnet");
        assert!(cfg.title.enabled);
        assert_eq!(cfg.title.max_length, 40);
    }

    #[test]
    fn unknown_field_rejected() {
        let src = r#"
            [title]
            mdoel = "sonnet"
        "#;
        let err = toml::from_str::<Config>(src).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

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
    }

    #[test]
    fn custom_agents_and_defaults_load() {
        let src = r#"
            [agents.pi-codex]
            kind = "pi"
            command = "pi --provider openai-codex --model gpt-5.5"
            use_bridge = true

            [projects]
            default_orchestrator_agent = "claude"
            default_worker_agent = "pi-codex"
            default_session_agent = "pi-codex"
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        let pi = cfg.agent("pi-codex").unwrap();
        assert_eq!(pi.kind, AgentKind::Pi);
        assert!(pi.use_bridge);
        assert_eq!(cfg.default_worker_agent_id(), "pi-codex");
        assert_eq!(cfg.default_session_agent_id(), "pi-codex");
    }
}
