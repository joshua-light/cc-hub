//! User config at `~/.cc-hub/config.toml`, loaded once and exposed via
//! [`get`]. Missing file, missing section, and missing field all fall back
//! to [`Default`], so this is a pure knob layer — removing the file yields
//! the same behaviour as shipped defaults.
//!
//! - [`agents`]: `[spawn]`, `[agents.<id>]` and the resolved agent list.
//! - [`sections`]: every other `[section]`.

use serde::de::IgnoredAny;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;

mod agents;
mod sections;

pub use agents::*;
pub use sections::*;

pub fn config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cc-hub").join("config.toml"))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub spawn: SpawnConfig,
    pub agents: BTreeMap<String, ConfiguredAgent>,
    pub projects: ProjectsConfig,
    pub tasks: TasksConfig,
    pub title: TitleConfig,
    pub inactive: InactiveConfig,
    pub scan: ScanConfig,
    pub ui: UiConfig,
    pub metrics: MetricsConfig,
    pub harness: HarnessConfig,
    pub builds: BuildsConfig,
    /// Retired with the Projects layer. Accepted and ignored so an old
    /// config keeps loading: `deny_unknown_fields` would otherwise fail the
    /// parse and drop every other setting back to defaults.
    #[serde(rename = "backlog")]
    _backlog: Option<IgnoredAny>,
    #[serde(rename = "auto_review")]
    _auto_review: Option<IgnoredAny>,
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
        assert!(!cfg.ui.show_planning_column);
    }

    /// Keys the Projects layer owned must not fail the parse: a parse error
    /// drops the whole config back to defaults.
    #[test]
    fn retired_projects_keys_are_ignored() {
        let src = r#"
            [backlog]
            enabled = true

            [auto_review]
            enabled = true
            interval_secs = 30

            [ui]
            show_projects_tab = true
            cell_width = 50

            [projects]
            default_orchestrator_agent = "claude"
        "#;
        let cfg: Config = toml::from_str(src).unwrap();
        assert_eq!(cfg.ui.cell_width, 50);
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
}
