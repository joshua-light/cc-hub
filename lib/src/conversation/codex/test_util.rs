//! JSON record builders for the Codex parser tests.

use serde_json::{json, Value};

pub(super) fn meta(session_id: &str, cwd: &str) -> Value {
    json!({
        "timestamp": "2026-07-14T13:22:22.467Z",
        "type": "session_meta",
        "payload": {"session_id": session_id, "cwd": cwd, "cli_version": "0.144.3"}
    })
}
pub(super) fn event(pt: &str, extra: Value) -> Value {
    let mut p = json!({"type": pt});
    if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            o.insert(k.clone(), v.clone());
        }
    }
    json!({"timestamp": "2026-07-14T13:22:42.639Z", "type": "event_msg", "payload": p})
}
pub(super) fn item(pt: &str, extra: Value) -> Value {
    let mut p = json!({"type": pt});
    if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            o.insert(k.clone(), v.clone());
        }
    }
    json!({"timestamp": "2026-07-14T13:22:50.000Z", "type": "response_item", "payload": p})
}
pub(super) fn turn_context(model: &str) -> Value {
    json!({"timestamp": "2026-07-14T13:22:47.950Z", "type": "turn_context",
        "payload": {"model": model, "turn_id": "t1"}})
}
pub(super) fn user_msg(text: &str) -> Value {
    event("user_message", json!({"message": text}))
}
pub(super) fn assistant_item(text: &str) -> Value {
    item(
        "message",
        json!({"role": "assistant", "content": [{"type": "output_text", "text": text}]}),
    )
}
