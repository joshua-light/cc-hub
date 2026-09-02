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

    /// Run `f` with `$HOME` pointing at a fresh tempdir, holding
    /// [`HOME_TEST_LOCK`] for the duration. The previous `$HOME` is restored
    /// even if `f` panics (drop guard), and a poisoned lock is recovered with
    /// `into_inner` so one failing test doesn't cascade `PoisonError`s into
    /// unrelated ones. Unix-only: on Windows `dirs::home_dir()` resolves via
    /// the profile API and ignores `$HOME`, so this redirection can't isolate
    /// anything there — gate callers behind `cfg(unix)`.
    #[cfg(unix)]
    pub fn with_temp_home<F: FnOnce()>(f: F) {
        struct RestoreHome(Option<std::ffi::OsString>);
        impl Drop for RestoreHome {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
        let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _restore = RestoreHome(std::env::var_os("HOME"));
        std::env::set_var("HOME", tmp.path());
        f();
    }
}
pub mod scanner;
pub mod send;
pub mod session_count;
pub mod session_index;
pub mod session_tasks;
pub mod spawn;
pub mod tasks;
pub mod title;
pub mod tmux_pane;
pub mod todo;
pub mod tool_use_count;
pub mod triage;
pub mod ui;
pub mod usage;
pub mod version;
pub mod watcher;

pub use ratatui_image;

use ratatui::Frame;

#[no_mangle]
pub fn render(frame: &mut Frame, app: &mut app::App) {
    ui::render(frame, app);
}
