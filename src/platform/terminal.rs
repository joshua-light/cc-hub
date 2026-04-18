//! Terminal-emulator launchers.
//!
//! Each supported emulator plugs in as a [`Launcher`]. [`pick`] returns the
//! first one that's available, honouring `$TERMINAL` when it matches a known
//! emulator. Adding a new emulator is a single struct + entry in
//! [`ALL_LAUNCHERS`].

use std::process::Command;

/// A terminal emulator we know how to spawn `ccyo` in.
pub trait Launcher: Send + Sync {
    fn name(&self) -> &'static str;

    /// True when the launcher's binary is on `$PATH`.
    fn is_available(&self) -> bool {
        has_binary(self.name())
    }

    /// argv to pass so the emulator opens in `cwd` and runs `shell -ic ccyo`.
    fn argv(&self, cwd: &str, shell: &str) -> Vec<String>;
}

pub struct Kitty;
pub struct Alacritty;
pub struct Foot;
pub struct Wezterm;
pub struct Ghostty;

impl Launcher for Kitty {
    fn name(&self) -> &'static str { "kitty" }
    fn argv(&self, cwd: &str, shell: &str) -> Vec<String> {
        vec![
            "--directory".into(), cwd.into(),
            shell.into(), "-ic".into(), "ccyo".into(),
        ]
    }
}

impl Launcher for Alacritty {
    fn name(&self) -> &'static str { "alacritty" }
    fn argv(&self, cwd: &str, shell: &str) -> Vec<String> {
        vec![
            "--working-directory".into(), cwd.into(),
            "-e".into(), shell.into(), "-ic".into(), "ccyo".into(),
        ]
    }
}

impl Launcher for Foot {
    fn name(&self) -> &'static str { "foot" }
    fn argv(&self, cwd: &str, shell: &str) -> Vec<String> {
        vec![
            format!("--working-directory={}", cwd),
            shell.into(), "-ic".into(), "ccyo".into(),
        ]
    }
}

impl Launcher for Wezterm {
    fn name(&self) -> &'static str { "wezterm" }
    fn argv(&self, cwd: &str, shell: &str) -> Vec<String> {
        vec![
            "start".into(), "--cwd".into(), cwd.into(),
            "--".into(), shell.into(), "-ic".into(), "ccyo".into(),
        ]
    }
}

impl Launcher for Ghostty {
    fn name(&self) -> &'static str { "ghostty" }
    fn argv(&self, cwd: &str, shell: &str) -> Vec<String> {
        vec![
            format!("--working-directory={}", cwd),
            "-e".into(), format!("{} -ic ccyo", shell),
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
