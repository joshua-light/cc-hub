//! Terminal-emulator launchers.
//!
//! Each supported emulator plugs in as a [`Launcher`]. [`pick`] returns the
//! first one that's available, honouring `$TERMINAL` when it matches a known
//! emulator. Adding a new emulator is a single struct + entry in
//! [`ALL_LAUNCHERS`].

use std::process::Command;

/// A terminal emulator we know how to spawn a shell command in.
pub trait Launcher: Send + Sync {
    fn name(&self) -> &'static str;

    /// True when the launcher's binary is on `$PATH`.
    fn is_available(&self) -> bool {
        has_binary(self.name())
    }

    /// argv to pass so the emulator opens in `cwd` and exec's `cmd_argv`
    /// directly, bypassing the user's shell. Used for self-contained commands
    /// (e.g. `tmux attach -t NAME`) to avoid interference from a shell rc
    /// that auto-launches tmux/tools.
    fn argv_bare(&self, cwd: &str, cmd_argv: &[&str]) -> Vec<String>;
}

pub struct Kitty;
pub struct Alacritty;
pub struct Foot;
pub struct Wezterm;
pub struct Ghostty;

impl Launcher for Kitty {
    fn name(&self) -> &'static str { "kitty" }
    fn argv_bare(&self, cwd: &str, cmd_argv: &[&str]) -> Vec<String> {
        let mut v = vec!["--directory".into(), cwd.into()];
        v.extend(cmd_argv.iter().map(|s| s.to_string()));
        v
    }
}

impl Launcher for Alacritty {
    fn name(&self) -> &'static str { "alacritty" }
    fn argv_bare(&self, cwd: &str, cmd_argv: &[&str]) -> Vec<String> {
        let mut v = vec![
            "--working-directory".into(), cwd.into(),
            "-e".into(),
        ];
        v.extend(cmd_argv.iter().map(|s| s.to_string()));
        v
    }
}

impl Launcher for Foot {
    fn name(&self) -> &'static str { "foot" }
    fn argv_bare(&self, cwd: &str, cmd_argv: &[&str]) -> Vec<String> {
        let mut v = vec![format!("--working-directory={}", cwd)];
        v.extend(cmd_argv.iter().map(|s| s.to_string()));
        v
    }
}

impl Launcher for Wezterm {
    fn name(&self) -> &'static str { "wezterm" }
    fn argv_bare(&self, cwd: &str, cmd_argv: &[&str]) -> Vec<String> {
        let mut v = vec![
            "start".into(), "--cwd".into(), cwd.into(),
            "--".into(),
        ];
        v.extend(cmd_argv.iter().map(|s| s.to_string()));
        v
    }
}

impl Launcher for Ghostty {
    fn name(&self) -> &'static str { "ghostty" }
    fn argv_bare(&self, cwd: &str, cmd_argv: &[&str]) -> Vec<String> {
        // Ghostty's -e takes a single command string it re-parses, so quote
        // each argv entry to survive.
        let quoted: Vec<String> = cmd_argv.iter().map(|s| shell_quote(s)).collect();
        vec![
            format!("--working-directory={}", cwd),
            "-e".into(), quoted.join(" "),
        ]
    }
}

/// Priority order matches the legacy hardcoded list — kitty/foot first because
/// those are the most common on the maintainers' boxes, alacritty next, then
/// the less-frequent options.
fn all_launchers() -> [Box<dyn Launcher>; 5] {
    [
        Box::new(Kitty),
        Box::new(Foot),
        Box::new(Alacritty),
        Box::new(Wezterm),
        Box::new(Ghostty),
    ]
}

/// Returns the launcher to use, preferring `$TERMINAL` when it's one of the
/// known emulators, then falling back to the first available in priority
/// order. None when no supported emulator is on PATH.
pub fn pick() -> Option<Box<dyn Launcher>> {
    if let Ok(t) = std::env::var("TERMINAL") {
        if !t.is_empty() {
            if let Some(l) = all_launchers()
                .into_iter()
                .find(|l| l.name() == t && l.is_available())
            {
                return Some(l);
            }
        }
    }
    all_launchers().into_iter().find(|l| l.is_available())
}

fn has_binary(name: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {} >/dev/null 2>&1", name)])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
