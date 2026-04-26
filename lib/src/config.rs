//! User config at `~/.cc-hub/config.toml`, loaded once and exposed via
//! [`get`]. Missing file, missing section, and missing field all fall back
//! to [`Default`], so this is a pure knob layer — removing the file yields
//! the same behaviour as shipped defaults.

use serde::Deserialize;
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
    pub title: TitleConfig,
    pub inactive: InactiveConfig,
    pub scan: ScanConfig,
    pub ui: UiConfig,
    pub metrics: MetricsConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpawnConfig {
    /// The command cc-hub invokes in each multiplexer pane. Resolved through
    /// the user's interactive shell so aliases / functions in their rc file
    /// expand — same contract as before config existed.
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
pub struct TitleConfig {
    /// Master switch for the background Haiku titler. When false, sessions
    /// display their first-user-message summary instead of a generated title.
    pub enabled: bool,
    /// Passed as `--model <model>` to the resolved spawn command.
    pub model: String,
    /// Clamp on the sanitized Haiku output; longer text is truncated at a
    /// utf8 boundary.
    pub max_length: usize,
    pub run_timeout_secs: u64,
    pub resolve_timeout_secs: u64,
    /// Max simultaneous in-flight `… -p` subprocesses. Keeps the pool from
    /// fork-storming on first scan with many untitled sessions.
    pub concurrency: usize,
    /// Prompt prepended to the first user message. Must end with a newline
    /// and a `Request:` marker or similar so Haiku has a cue.
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
    "Output a 2 or 3 word title summarizing this Claude Code user request. \
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
    /// How long a dead session's JSONL stays visible after its last touch.
    pub window_secs: u64,
    /// Per-cwd cap, ranked by mtime, so a project with hundreds of old
    /// JSONLs doesn't dominate a scan tick.
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
    /// Fallback timer that catches PID deaths and missed fs events.
    pub fs_fallback_interval_secs: u64,
    /// How often to re-fetch the Anthropic usage API.
    pub usage_refresh_interval_secs: u64,
    /// How long the on-disk usage response is trusted before re-curl'ing.
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
            log::warn!("config: read error at {}: {} — using defaults", path.display(), e);
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
    }

    #[test]
    fn partial_section_merges_with_defaults() {
        let src = r#"
            [title]
            model = "sonnet"
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        assert_eq!(cfg.title.model, "sonnet");
        // untouched fields keep defaults
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
    fn full_config_round_trips() {
        let src = r#"
            [spawn]
            command = "my-claude"

            [title]
            enabled = false
            model = "sonnet"
            max_length = 60
            run_timeout_secs = 30
            resolve_timeout_secs = 5
            concurrency = 4
            prompt = "hi"

            [inactive]
            window_secs = 86400
            max_per_project = 3

            [scan]
            fs_fallback_interval_secs = 5
            usage_refresh_interval_secs = 30
            usage_cache_ttl_secs = 30

            [ui]
            status_msg_ttl_secs = 3
            pending_dispatch_timeout_secs = 90
            cell_height = 10
            cell_width = 50

            [metrics]
            min_growth_turns = 10
            growth_threshold = 4.5
            top_interruptions = 5
            top_growth_findings = 5
            top_peak_context_findings = 5
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        assert_eq!(cfg.spawn.command, "my-claude");
        assert!(!cfg.title.enabled);
        assert_eq!(cfg.title.model, "sonnet");
        assert_eq!(cfg.inactive.window_secs, 86400);
        assert_eq!(cfg.scan.fs_fallback_interval_secs, 5);
        assert_eq!(cfg.ui.cell_width, 50);
        assert_eq!(cfg.metrics.growth_threshold, 4.5);
    }
}
