use cc_hub_lib::app::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// F1 and Ctrl+Shift+V belong to the hub; every other key goes to the pane.
pub(super) fn handle(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::F(1) => {
            app.release_pane();
        }
        KeyCode::Char(c)
            if (c == 'v' || c == 'V')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && key.modifiers.contains(KeyModifiers::SHIFT) =>
        {
            let status = match cc_hub_lib::clipboard::paste() {
                Ok(text) if text.is_empty() => Some("clipboard empty".to_string()),
                Ok(text) => match app.focused_pane() {
                    Some(pane) => pane
                        .paste_text(&text)
                        .err()
                        .map(|e| format!("paste failed: {}", e)),
                    None => None,
                },
                Err(e) => Some(format!("paste failed: {}", e)),
            };
            if let Some(msg) = status {
                app.set_status(msg);
            }
        }
        _ => {
            if let Some(pane) = app.focused_pane() {
                pane.send_key(key);
            }
        }
    }
}
