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

/// A Claude session with every optional field empty. Fixtures override the
/// fields they care about with struct-update syntax
/// (`SessionInfo { state, ..session_info() }`).
pub fn session_info() -> crate::models::SessionInfo {
    crate::models::SessionInfo {
        agent_id: "claude".into(),
        agent_kind: crate::agent::AgentKind::Claude,
        pid: 1,
        session_id: "s".into(),
        cwd: "/tmp".into(),
        project_name: "tmp".into(),
        started_at: 0,
        last_activity: None,
        state: crate::models::SessionState::Idle,
        last_user_message: None,
        summary: None,
        title: None,
        titling: false,
        model: None,
        git_branch: None,
        version: None,
        jsonl_path: None,
        tmux_session: None,
        current_tool: None,
        is_thinking: false,
        context_tokens: None,
        tool_uses_count: 0,
    }
}
