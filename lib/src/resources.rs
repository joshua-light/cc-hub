//! Account metadata shared with the bundled broker. Never reads credentials.
use crate::agent::{AgentConfig, AgentKind};
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize)]
pub struct Account {
    pub provider: AgentKind,
    pub home: Option<String>,
    pub home_mode: Option<String>,
    pub executable: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Default, Deserialize)]
struct Registry {
    #[serde(default)]
    accounts: BTreeMap<String, Account>,
}

pub fn accounts() -> BTreeMap<String, Account> {
    let path = std::env::var_os("CC_HUB_RESOURCE_CONFIG")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".cc-hub/resources.toml")));
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| toml::from_str::<Registry>(&s).ok())
        .unwrap_or_default()
        .accounts
}

impl Account {
    pub fn home(&self) -> Option<PathBuf> {
        self.home
            .as_deref()
            .map(crate::platform::paths::expand_home)
            .or_else(|| {
                dirs::home_dir().map(|h| {
                    h.join(if self.provider == AgentKind::Codex {
                        ".codex"
                    } else {
                        ".claude"
                    })
                })
            })
    }
    pub fn apply(&self, command: &mut std::process::Command) {
        for key in [
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "OPENAI_API_KEY",
            "CODEX_API_KEY",
            "CODEX_ACCESS_TOKEN",
            "ANTHROPIC_BASE_URL",
            "OPENAI_BASE_URL",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ] {
            command.env_remove(key);
        }
        if let Some(home) = self.home() {
            if self.provider == AgentKind::Codex {
                command.env("CODEX_HOME", home);
            } else if self.home_mode.as_deref() != Some("default") {
                command.env("CLAUDE_CONFIG_DIR", home);
            }
        }
    }
    pub fn agent(&self, id: &str) -> AgentConfig {
        let exe = self.executable.as_deref().unwrap_or(match self.provider {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Pi => "pi",
        });
        let command = std::iter::once(exe)
            .chain(self.args.iter().map(String::as_str))
            .map(crate::platform::terminal::shell_quote)
            .collect::<Vec<_>>()
            .join(" ");
        AgentConfig {
            id: id.into(),
            kind: self.provider,
            command,
            use_bridge: false,
            models: if self.provider == AgentKind::Claude {
                crate::agent::default_claude_models()
            } else {
                Vec::new()
            },
        }
    }
}

pub fn for_agent(id: &str) -> Option<Account> {
    let cfg = crate::config::get();
    let name = cfg
        .agents
        .get(id)
        .and_then(|a| a.account.as_deref())
        .unwrap_or(id);
    accounts().remove(name)
}

pub fn task_kind(task: &str) -> Option<String> {
    let directory = std::env::var_os("CC_HUB_RESOURCE_DIR")
        .map(PathBuf::from)
        .or_else(|| crate::platform::paths::cc_hub_home().map(|h| h.join("resources")))?;
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(directory.join("state.json")).ok()?).ok()?;
    value["workers"].as_object()?.values().find(|w| {
        w["task"].as_str() == Some(task)
            && w["role"].as_str() == Some("dev")
            && w["status"].as_str() != Some("complete")
    })?["kind"]
        .as_str()
        .map(str::to_string)
}

thread_local! { static CLAUDE_HOME: RefCell<Option<PathBuf>> = const { RefCell::new(None) }; }
pub fn claude_scan_home() -> Option<PathBuf> {
    CLAUDE_HOME.with(|p| p.borrow().clone())
}
pub fn with_claude_home<T>(home: PathBuf, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<PathBuf>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CLAUDE_HOME.with(|p| {
                p.replace(self.0.take());
            });
        }
    }
    let _restore = Restore(CLAUDE_HOME.with(|p| p.replace(Some(home))));
    f()
}
pub fn codex_roots() -> Vec<PathBuf> {
    let mut roots: Vec<_> = crate::platform::paths::codex_sessions_dir()
        .into_iter()
        .collect();
    for account in accounts()
        .values()
        .filter(|a| a.provider == AgentKind::Codex)
    {
        if let Some(path) = account.home().map(|h| h.join("sessions")) {
            if !roots.contains(&path) {
                roots.push(path);
            }
        }
    }
    roots
}
pub fn label_sessions(sessions: &mut [crate::models::SessionInfo]) {
    let accounts = accounts();
    for session in sessions {
        if let Some(path) = session.jsonl_path.as_ref() {
            if let Some((id, _)) = accounts.iter().find(|(_, a)| {
                a.provider == session.agent_kind && a.home().is_some_and(|h| path.starts_with(h))
            }) {
                session.agent_id = id.clone();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_claude_removes_override_instead_of_relocating_state() {
        let account: Account = toml::from_str("provider='claude'\nhome_mode='default'").unwrap();
        let mut command = std::process::Command::new("claude");
        command.env("CLAUDE_CONFIG_DIR", "/wrong");
        account.apply(&mut command);
        assert!(command
            .get_envs()
            .any(|(key, value)| key == "CLAUDE_CONFIG_DIR" && value.is_none()));
    }
    #[test]
    fn scan_scope_restores_on_nested_calls() {
        with_claude_home(PathBuf::from("/one"), || {
            with_claude_home(PathBuf::from("/two"), || {
                assert_eq!(claude_scan_home(), Some(PathBuf::from("/two")))
            });
            assert_eq!(claude_scan_home(), Some(PathBuf::from("/one")));
        });
        assert_eq!(claude_scan_home(), None);
    }
}
