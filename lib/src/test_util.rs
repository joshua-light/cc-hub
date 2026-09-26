//! Shared `$HOME`-mutating test mutex. Several modules' tests redirect
//! `$HOME` at a tempdir to exercise filesystem helpers; without a
//! cross-module lock they race on the global env var.
use std::sync::Mutex;
pub static HOME_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Every variable that can point a lookup back out of the temp home.
/// `$HOME` alone is not enough: each of these overrides it somewhere, so
/// a test that leaves one standing reads the real machine and passes or
/// fails by where it was run. `CLAUDE_CONFIG_DIR` relocates the whole
/// Claude layout (`--claude-config-dir` sessions export it). The
/// `CC_HUB_RESOURCE_*` pair is exported into every session the hub
/// starts, which is exactly where the suite is run — leaving them set
/// made `resources::accounts()` find the real registry inside a temp home.
#[cfg(unix)]
const REDIRECTED: [&str; 5] = [
    "HOME",
    "CODEX_HOME",
    "CLAUDE_CONFIG_DIR",
    "CC_HUB_RESOURCE_CONFIG",
    "CC_HUB_RESOURCE_DIR",
];

/// Run `f` with `$HOME` pointing at a fresh tempdir and [`REDIRECTED`]
/// cleared, holding [`HOME_TEST_LOCK`] for the duration. The previous
/// environment is restored even if `f` panics (drop guard), and a
/// poisoned lock is recovered with `into_inner` so one failing test
/// doesn't cascade `PoisonError`s into unrelated ones. Unix-only: on
/// Windows `dirs::home_dir()` resolves via the profile API and ignores
/// `$HOME`, so this redirection can't isolate anything there — gate
/// callers behind `cfg(unix)`.
#[cfg(unix)]
pub fn with_temp_home<F: FnOnce()>(f: F) {
    struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (name, before) in self.0.drain(..) {
                match before {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
    let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let _restore = Restore(
        REDIRECTED
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect(),
    );
    for name in REDIRECTED {
        std::env::remove_var(name);
    }
    std::env::set_var("HOME", tmp.path());
    f();
}
