//! The remaining `config.toml` sections: one struct per `[section]`.

use serde::de::IgnoredAny;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectsConfig {
    pub default_session_agent: Option<String>,
    /// Retired with the Projects layer; accepted and ignored (see
    /// [`Config`](super::Config)'s `_backlog`).
    #[serde(rename = "default_orchestrator_agent")]
    _default_orchestrator_agent: Option<IgnoredAny>,
}

/// `[tasks]` — the personal board's own knobs.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TasksConfig {
    /// The deliverable kinds a card may be given on the board (`T`), in the
    /// order the picker offers them. cc-hub never interprets a kind: it stores
    /// the word and hands it to whoever opens the session — the task router
    /// owns where each one lands. Empty (the default) means the board offers
    /// nothing to pick and every card stays router-classified.
    pub kinds: Vec<String>,
    /// The kinds whose task is worked by two sessions in sequence: one
    /// builds, a second tests what was built and opens the pull request.
    /// A kind outside this list has the one session — it checks its own work
    /// and delivers — so a `role=verification` link naming it hands the card
    /// to a role that does not exist, and is refused. Empty (the default)
    /// leaves every kind free to hand over, as before the list existed.
    pub handover_kinds: Vec<String>,
}

impl TasksConfig {
    /// `kind` as the board would accept it from `T`, or why not: a word
    /// outside the configured list, or a board with no list at all. Every
    /// caller that files a card with a kind goes through this — the CLI, a
    /// link — so a kind on a card is always one the picker offers.
    pub fn known_kind(&self, kind: &str) -> Result<String, String> {
        if self.kinds.iter().any(|k| k == kind) {
            return Ok(kind.to_string());
        }
        Err(if self.kinds.is_empty() {
            "no task kinds configured — set [tasks].kinds in ~/.cc-hub/config.toml".into()
        } else {
            format!("{} is not one of {}", kind, self.kinds.join(", "))
        })
    }

    /// Whether a card of this kind is handed to a verification session. A
    /// card with no kind is nobody's to refuse: nothing has classified it
    /// yet, so it keeps the freedom it had before the list existed.
    pub fn hands_over(&self, kind: Option<&str>) -> bool {
        self.handover_kinds.is_empty()
            || kind.is_none_or(|k| self.handover_kinds.iter().any(|h| h == k))
    }
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
    /// The Planning column on the Tasks board. Off by default; set true to
    /// show it. When off, its cards fold into In Progress, so plan-ready work
    /// stays visible and Space still approves it (the action keys off the
    /// card's status, not the column it renders in).
    pub show_planning_column: bool,
    /// Retired with the Projects layer; accepted and ignored (see
    /// [`Config`](super::Config)'s `_backlog`).
    #[serde(rename = "show_projects_tab")]
    _show_projects_tab: Option<IgnoredAny>,
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
            _show_projects_tab: None,
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

/// The Builds tab (`lib/src/builds/`). It shows once a recipe exists.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BuildsConfig {
    pub recipes: BTreeMap<String, crate::builds::Recipe>,
    /// How often the TUI re-reads builds from disk.
    pub refresh_secs: u64,
    /// How often it asks each recipe's `current` what its last run left in
    /// place, and the broker who holds each recipe's resource. Both cost a process or an
    /// ssh round trip, so this is slower than the refresh.
    pub probe_secs: u64,
}

impl BuildsConfig {
    pub fn refresh(&self) -> Duration {
        Duration::from_secs(self.refresh_secs.max(1))
    }

    pub fn probe(&self) -> Duration {
        Duration::from_secs(self.probe_secs.max(5))
    }
}

impl Default for BuildsConfig {
    fn default() -> Self {
        Self {
            recipes: BTreeMap::new(),
            refresh_secs: 1,
            probe_secs: 15,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;

    #[test]
    fn only_a_listed_kind_hands_over() {
        let src = r#"
            [tasks]
            kinds = ["tps", "basic"]
            handover_kinds = ["tps"]
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        assert!(cfg.tasks.hands_over(Some("tps")));
        assert!(!cfg.tasks.hands_over(Some("basic")));
        assert!(cfg.tasks.hands_over(None));
    }

    #[test]
    fn an_empty_handover_list_holds_nobody_back() {
        let cfg: Config = toml::from_str("[tasks]\nkinds = [\"basic\"]").unwrap();
        assert!(cfg.tasks.hands_over(Some("basic")));
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
}
