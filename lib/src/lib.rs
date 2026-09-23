pub mod acks;
pub mod agent;
pub mod agent_runtime;
pub mod app;
pub mod auto_review;
pub mod bookmarks;
pub mod clipboard;
pub mod codex_conversation;
pub mod codex_scanner;
pub mod config;
pub mod conversation;
pub mod dir_cache;
pub mod focus;
pub mod folder_picker;
pub mod fuzzy;
pub mod gh;
pub mod harness;
pub mod link;
pub mod live_view;
pub mod merge_lock;
pub mod metrics;
pub mod models;
pub mod ops;
pub mod orchestrator;
pub mod persist;
pub mod pi_bridge;
pub mod pi_conversation;
pub mod pi_scanner;
pub mod platform;
pub mod pr;
pub mod projects_scan;

#[cfg(test)]
pub(crate) mod test_util {
    //! Shared `$HOME`-mutating test mutex. Several modules' tests redirect
    //! `$HOME` at a tempdir to exercise filesystem helpers; without a
    //! cross-module lock they race on the global env var.
    use std::sync::Mutex;
    pub static HOME_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Every variable that can point a lookup back out of the temp home.
    /// `$HOME` alone is not enough: each of these overrides it somewhere, so
    /// a test that leaves one standing reads the real machine and passes or
    /// fails by where it was run. The `CC_HUB_RESOURCE_*` pair is exported
    /// into every session the hub starts, which is exactly where the suite
    /// is run — leaving them set made `resources::accounts()` find the real
    /// registry inside a temp home.
    #[cfg(unix)]
    const REDIRECTED: [&str; 4] = [
        "HOME",
        "CODEX_HOME",
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
}
pub mod resources;
pub mod respawn;
pub mod scanner;
pub mod send;
pub mod session_count;
pub mod session_index;
pub mod session_tasks;
pub mod spawn;
pub mod task_activity;
pub mod task_stats;
pub mod tasks;
pub mod title;
pub mod tmux_pane;
pub mod tool_use_count;
pub mod triage;
pub mod ui;
pub mod usage;
pub mod version;
pub mod wake;
pub mod watcher;

pub use ratatui_image;

use ratatui::Frame;

#[no_mangle]
pub fn render(frame: &mut Frame, app: &mut app::App) {
    ui::render(frame, app);
}
