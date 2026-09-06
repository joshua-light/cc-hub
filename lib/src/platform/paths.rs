//! Cache / config paths.
//!
//! Centralised so callers don't bake `/tmp` or `~/.config/<compositor>` into
//! unrelated modules.

use std::path::PathBuf;

/// Cache file for the Anthropic usage API response. Pinned to `/tmp` because
/// it's a cross-process contract with an external statusline helper that
/// reads the same path. Changing this location is a breaking change — so the
/// default-account path is left untouched, and only a non-default
/// `CLAUDE_CONFIG_DIR` gets a per-account suffix so parallel cc-hub instances
/// (one per account) don't clobber each other's cached usage.
pub fn usage_cache_file() -> PathBuf {
    match claude_config_dir() {
        Some(dir) => {
            let tag = config_dir_tag(&dir);
            PathBuf::from(format!("/tmp/claude-statusline-usage.{}.json", tag))
        }
        None => PathBuf::from("/tmp/claude-statusline-usage.json"),
    }
}

/// A filesystem-safe tag derived from a config-dir path, used to namespace
/// shared `/tmp` artifacts per account.
fn config_dir_tag(dir: &std::path::Path) -> String {
    dir.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Cache directory for cc-hub. Falls back to `/tmp` when `dirs::cache_dir`
/// can't resolve a home — matches the previous log-path behaviour.
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("cc-hub")
}

/// `~/x` → `<home>/x`; anything else is taken as given. Config and links
/// are written by hand, so a leading `~` is a path the user expects to work.
pub fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/").zip(dirs::home_dir()) {
        Some((rest, home)) => home.join(rest),
        None => PathBuf::from(p),
    }
}

pub fn cc_hub_home() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cc-hub"))
}

/// An explicit Claude config-dir override, mirroring Claude Code's own
/// `CLAUDE_CONFIG_DIR` env var. When set, Claude relocates its entire user
/// data tree (`sessions/`, `projects/`, `history.jsonl`, `.credentials.json`)
/// *and* the `.claude.json` state file into this directory. cc-hub honours the
/// same variable so a single instance reads/writes exactly the account Claude
/// itself would use — letting you run one cc-hub per account in parallel.
///
/// `None` (env unset or empty) means the default `~/.claude` layout. The
/// `--claude-config-dir` CLI flag is sugar that simply sets this env var (so
/// directly-spawned `claude -p` children inherit it too).
pub fn claude_config_dir() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => None,
    }
}

/// Claude Code's user data directory: the `CLAUDE_CONFIG_DIR` override when
/// set, otherwise `~/.claude`. None when neither is resolvable (very unusual —
/// daemons without HOME, broken chroots).
pub fn claude_home() -> Option<PathBuf> {
    claude_config_dir().or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
}

/// Path to Claude's `.claude.json` state file (holds the `projects` map with
/// `hasTrustDialogAccepted`). With a config-dir override it lives *inside* that
/// dir; by default it sits at the home root (`~/.claude.json`), NOT under
/// `~/.claude/`.
pub fn claude_config_json() -> Option<PathBuf> {
    match claude_config_dir() {
        Some(dir) => Some(dir.join(".claude.json")),
        None => dirs::home_dir().map(|h| h.join(".claude.json")),
    }
}

/// Path to Claude's OAuth credentials file, which lives inside the data dir.
/// (On macOS the token is usually in the Keychain instead, so this may be
/// absent — callers treat that as "no token".)
pub fn claude_credentials_file() -> Option<PathBuf> {
    claude_home().map(|d| d.join(".credentials.json"))
}

/// Pi's user data directory (`~/.pi/agent`). None when home is unresolvable.
pub fn pi_home() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".pi").join("agent"))
}

pub fn pi_sessions_dir() -> Option<PathBuf> {
    pi_home().map(|h| h.join("sessions"))
}

/// Codex CLI's user data directory (`~/.codex`). None when home is
/// unresolvable.
pub fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))
}

/// Codex rollout transcripts live under `~/.codex/sessions/YYYY/MM/DD/` as
/// `rollout-<ts>-<uuid>.jsonl` — one file per session, nested three levels
/// deep by date (unlike Pi's flat per-project layout).
pub fn codex_sessions_dir() -> Option<PathBuf> {
    codex_home().map(|h| h.join("sessions"))
}

pub fn pi_bridge_file() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("pi-bridge.ts"))
}

pub fn pi_heartbeats_dir() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("pi-heartbeats"))
}

/// Optional user-provided wrapper script for a terminal emulator under the
/// Hyprland dotfiles layout (`~/.config/hypr/scripts/<name>`). Many users
/// configure their SUPER+Enter binding to invoke such a script so the terminal
/// is launched with a personalised `--config-file`; honouring it here keeps
/// cc-hub's spawned windows visually consistent with their normal terminals.
pub fn terminal_wrapper_script(name: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let p = PathBuf::from(home).join(".config/hypr/scripts").join(name);
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::HOME_TEST_LOCK;

    fn with_config_dir<F: FnOnce()>(value: Option<&str>, f: F) {
        let _guard = HOME_TEST_LOCK.lock().unwrap();
        let prev = std::env::var_os("CLAUDE_CONFIG_DIR");
        match value {
            Some(v) => std::env::set_var("CLAUDE_CONFIG_DIR", v),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
        f();
        match prev {
            Some(v) => std::env::set_var("CLAUDE_CONFIG_DIR", v),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
    }

    #[test]
    fn override_relocates_data_and_state_into_config_dir() {
        with_config_dir(Some("/tmp/cc-hub-test-personal"), || {
            let base = PathBuf::from("/tmp/cc-hub-test-personal");
            assert_eq!(claude_config_dir(), Some(base.clone()));
            assert_eq!(claude_home(), Some(base.clone()));
            // .claude.json lives INSIDE the config dir when overridden.
            assert_eq!(claude_config_json(), Some(base.join(".claude.json")));
            assert_eq!(
                claude_credentials_file(),
                Some(base.join(".credentials.json"))
            );
            // /tmp usage cache is namespaced so parallel instances don't clash.
            assert_ne!(
                usage_cache_file(),
                PathBuf::from("/tmp/claude-statusline-usage.json")
            );
        });
    }

    #[test]
    fn default_layout_is_unchanged_without_override() {
        with_config_dir(None, || {
            assert_eq!(claude_config_dir(), None);
            // .claude.json sits at the home ROOT by default, not under ~/.claude.
            let home = dirs::home_dir().unwrap();
            assert_eq!(claude_home(), Some(home.join(".claude")));
            assert_eq!(claude_config_json(), Some(home.join(".claude.json")));
            // Cache path is the pinned cross-process contract.
            assert_eq!(
                usage_cache_file(),
                PathBuf::from("/tmp/claude-statusline-usage.json")
            );
        });
    }

    #[test]
    fn empty_override_falls_back_to_default() {
        with_config_dir(Some(""), || {
            assert_eq!(claude_config_dir(), None);
        });
    }
}
