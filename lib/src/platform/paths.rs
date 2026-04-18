//! Cache / config paths.
//!
//! Centralised so callers don't bake `/tmp` or `~/.config/<compositor>` into
//! unrelated modules.

use std::path::PathBuf;

/// Cache file for the Anthropic usage API response. Pinned to `/tmp` because
/// it's a cross-process contract with an external statusline helper that
/// reads the same path. Changing this location is a breaking change.
pub fn usage_cache_file() -> PathBuf {
    PathBuf::from("/tmp/claude-statusline-usage.json")
}

/// Cache directory for cc-hub. Falls back to `/tmp` when `dirs::cache_dir`
/// can't resolve a home — matches the previous log-path behaviour.
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("cc-hub")
}

/// Claude Code's user data directory (`~/.claude`). None when home is
/// unresolvable (very unusual — daemons without HOME, broken chroots).
pub fn claude_home() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude"))
}

/// Optional user-provided wrapper script for a terminal emulator under the
/// Hyprland dotfiles layout (`~/.config/hypr/scripts/<name>`). Many users
/// configure their SUPER+Enter binding to invoke such a script so the terminal
/// is launched with a personalised `--config-file`; honouring it here keeps
/// cc-hub's spawned windows visually consistent with their normal terminals.
pub fn terminal_wrapper_script(name: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let p = PathBuf::from(home)
        .join(".config/hypr/scripts")
        .join(name);
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}
