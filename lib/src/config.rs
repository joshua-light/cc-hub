//! User config at `~/.cc-hub/config.toml`, loaded once and exposed via
//! [`get`]. Missing file, missing section, and missing field all fall back
//! to [`Default`], so this is a pure knob layer — removing the file yields
//! the same behaviour as shipped defaults.

use crate::agent::{default_claude_models, AgentConfig, AgentKind, AgentModel};
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
    pub auto_review: AutoReviewConfig,
    pub harness: HarnessConfig,
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
    pub models: Vec<ConfiguredModel>,
}

impl Default for ConfiguredAgent {
    fn default() -> Self {
        Self {
            kind: AgentKind::Claude,
            command: "cc-hub-new".into(),
            use_bridge: false,
            models: Vec::new(),
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

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectsConfig {
    pub default_orchestrator_agent: Option<String>,
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
    /// TTL (seconds) for the per-directory listing cache used by the orphan /
    /// inactive-session walks. A project dir whose mtime is unchanged is
    /// re-listed at most once per this many seconds instead of every scan
    /// tick. A new file bumps the dir mtime and invalidates immediately, so
    /// this only bounds how stale an *otherwise-unchanged* listing may get.
    pub orphan_relist_secs: u64,
}

impl Default for InactiveConfig {
    fn default() -> Self {
        Self {
            window_secs: 3 * 86_400,
            max_per_project: 5,
            orphan_relist_secs: 30,
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
    /// The Projects tab (orchestrator kanban) is WIP and hidden from the
    /// tab strip + ⇥ cycle by default. Set true to bring it back.
    pub show_projects_tab: bool,
    /// The Planning column on the Tasks board. Off by default; set true to
    /// show it. When off, its cards fold into In Progress, so plan-ready work
    /// stays visible and Space still approves it (the action keys off the
    /// card's status, not the column it renders in).
    pub show_planning_column: bool,
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
            // 6 = borders + payload row + branch row + model row + footer
            // row: every body row carries content, so taller cells would
            // just render blank rows. At 5 and below the renderer merges
            // branch/model/id into one compact row.
            cell_height: 6,
            cell_width: 42,
            show_projects_tab: false,
            show_planning_column: false,
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

/// Background auto-review tick. When enabled, every `interval_secs` cc-hub
/// scans for tasks in Review state with an Open / ChangesRequested PR that
/// haven't been auto-reviewed this round yet, and spawns ONE reviewer
/// session for the oldest eligible task. The reviewer is a real agent
/// session (read-only) that can build, test, post comments, and either
/// approve or request changes via the existing `cc-hub pr ...` CLI verbs —
/// closing the orchestrator → review → iterate → re-review loop without a
/// human in the path.
///
/// Off by default: each tick spawns a billed agent session.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutoReviewConfig {
    pub enabled: bool,
    /// Agent backend to use for reviewer sessions. Resolved against
    /// `[agents.*]`; falls back to the default orchestrator agent if unset.
    pub agent: Option<String>,
    pub interval_secs: u64,
    /// Per-tick eligibility cap: don't re-review a task whose
    /// `last_auto_reviewed_at` is within this many seconds. Belt-and-braces
    /// alongside the per-round clear-on-re-entry gate.
    pub ttl_secs: u64,
    pub run_timeout_secs: u64,
    /// Cap on PR comments rendered into the reviewer briefing. Long iterative
    /// review rounds otherwise grow the prompt without bound; older comments
    /// are dropped with a `(+N older comments not shown)` footer so the
    /// reviewer knows context exists.
    pub max_comments_in_prompt: u32,
}

impl AutoReviewConfig {
    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs)
    }
    pub fn run_timeout(&self) -> Duration {
        Duration::from_secs(self.run_timeout_secs)
    }
}

impl Default for AutoReviewConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            agent: None,
            interval_secs: 30,
            ttl_secs: 600,
            run_timeout_secs: 1800,
            max_comments_in_prompt: 8,
        }
    }
}

/// Persistent agents (the Agents tab, `lib/src/harness/`). The supervisor
/// runs inside the TUI and only spends money on agents that exist under
/// `~/.cc-hub/agents/` and are enabled, so it is on by default.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HarnessConfig {
    /// Run the supervisor loop inside the TUI.
    pub enabled: bool,
    /// Show the Agents tab. It is also hidden while `~/.cc-hub/agents/`
    /// does not exist.
    pub show_tab: bool,
    /// How often the TUI re-reads agent state from disk.
    pub refresh_secs: u64,
}

impl HarnessConfig {
    pub fn refresh(&self) -> Duration {
        Duration::from_secs(self.refresh_secs.max(1))
    }
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            show_tab: true,
            refresh_secs: 3,
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
        assert_eq!(cfg.inactive.orphan_relist_secs, 30);
        assert_eq!(cfg.default_orchestrator_agent_id(), "claude");
        assert!(!cfg.ui.show_planning_column);
    }

    #[test]
    fn planning_column_can_be_enabled_explicitly() {
        let src = r#"
            [ui]
            show_planning_column = true
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        assert!(cfg.ui.show_planning_column);
    }

    #[test]
    fn inactive_orphan_relist_secs_overrides() {
        let src = r#"
            [inactive]
            orphan_relist_secs = 5
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        assert_eq!(cfg.inactive.orphan_relist_secs, 5);
        // Sibling fields keep their defaults.
        assert_eq!(cfg.inactive.max_per_project, 5);
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
        assert_eq!(cfg.default_orchestrator_agent_id(), "claude");
        assert_eq!(cfg.default_session_agent_id(), "pi-codex");
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
