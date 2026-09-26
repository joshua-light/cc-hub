use cc_hub_lib::app::App;
use cc_hub_lib::{live_view, models, platform};
use crossterm::event::{KeyCode, KeyEvent};

pub(super) fn handle(app: &mut App, key: KeyEvent, spawn_metrics: &impl Fn()) {
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => {
            app.metrics_nav_down();
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.metrics_nav_up();
        }
        KeyCode::Enter => {
            if let Some(row) = app.selected_metrics_session().cloned() {
                let agent_kind = if platform::paths::pi_sessions_dir()
                    .as_ref()
                    .is_some_and(|dir| row.jsonl_path.starts_with(dir))
                {
                    cc_hub_lib::agent::AgentKind::Pi
                } else {
                    cc_hub_lib::agent::AgentKind::Claude
                };
                let lv = live_view::LiveView::review(
                    row.jsonl_path.clone(),
                    agent_kind,
                    row.peak_timestamp_ms,
                );
                if lv.messages.is_empty() {
                    app.set_status(format!(
                        "can't open {}: {} missing or empty",
                        models::short_sid(&row.session_id),
                        row.jsonl_path.display()
                    ));
                } else {
                    app.enter_live_tail(lv);
                }
            }
        }
        KeyCode::Char('r') => {
            app.metrics.analysis = None;
            spawn_metrics();
        }
        _ => {}
    }
}
