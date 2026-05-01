pub mod acks;
pub mod agent;
pub mod app;
pub mod auto_review;
pub mod clipboard;
pub mod config;
pub mod conversation;
pub mod focus;
pub mod folder_picker;
pub mod gh;
pub mod live_view;
pub mod merge_lock;
pub mod metrics;
pub mod models;
pub mod orchestrator;
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
}
pub mod scanner;
pub mod send;
pub mod spawn;
pub mod title;
pub mod tmux_pane;
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
