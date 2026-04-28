pub mod acks;
pub mod app;
pub mod clipboard;
pub mod config;
pub mod conversation;
pub mod focus;
pub mod folder_picker;
pub mod gh;
pub mod live_view;
pub mod metrics;
pub mod models;
pub mod orchestrator;
pub mod platform;
pub mod reservations;
pub mod projects_scan;
pub mod scanner;
pub mod send;
pub mod spawn;
pub mod title;
pub mod tmux_pane;
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
