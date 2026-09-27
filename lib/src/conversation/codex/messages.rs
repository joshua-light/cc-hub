use super::{payload, payload_type, rec_type, record_timestamp};
use crate::conversation::render::truncate_plain;
use crate::conversation::{full_text, NO_TEXT_CONTENT};
use crate::models::ConversationMessage;
use serde_json::Value;

/// The text of a codex chat message — user `input_text` blocks or assistant
/// `output_text` blocks joined together.
fn message_text(msg: &Value, max_len: usize) -> Option<String> {
    let content = msg.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(truncate_plain(s, max_len));
    }
    let arr = content.as_array()?;
    let mut parts = Vec::new();
    for block in arr {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("input_text") | Some("output_text") | Some("text") => {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    parts.push(t);
                }
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(truncate_plain(&parts.join(" "), max_len))
    }
}

/// The text a `user_message` event carries directly under `payload.message`.
fn user_event_text(entry: &Value, max_len: usize) -> Option<String> {
    payload(entry)?
        .get("message")
        .and_then(|v| v.as_str())
        .map(|s| truncate_plain(s, max_len))
}

fn is_user_message_record(entry: &Value) -> bool {
    (rec_type(entry) == Some("event_msg") && payload_type(entry) == Some("user_message"))
        || (rec_type(entry) == Some("response_item")
            && payload_type(entry) == Some("message")
            && payload(entry)
                .and_then(|p| p.get("role"))
                .and_then(|r| r.as_str())
                == Some("user"))
}

fn user_record_text(entry: &Value, max_len: usize) -> Option<String> {
    if rec_type(entry) == Some("event_msg") {
        return user_event_text(entry, max_len);
    }
    message_text(payload(entry)?, max_len)
}

/// Codex persists the injected environment bootstrap as a user-shaped record.
/// It is useful to Codex itself, but it is not the user's request and must not
/// become the session's visible name/message in the hub.
fn is_environment_context(text: &str) -> bool {
    text.trim_start().starts_with("<environment_context>")
}

fn displayable_user_text(entry: &Value, max_len: usize) -> Option<String> {
    let text = user_record_text(entry, max_len)?;
    (!is_environment_context(&text)).then_some(text)
}

pub fn extract_last_user_message(entries: &[Value]) -> Option<String> {
    entries
        .iter()
        .rev()
        .filter(|e| is_user_message_record(e))
        .find_map(|e| displayable_user_text(e, 200))
}

pub fn extract_first_user_message(entries: &[Value]) -> Option<String> {
    entries
        .iter()
        .filter(|e| is_user_message_record(e))
        .find_map(|e| displayable_user_text(e, 200))
}

/// Codex's side of [`crate::conversation::extract_last_assistant_message`]:
/// the newest assistant `message` response item with text, whole.
pub fn extract_last_assistant_message(entries: &[Value]) -> Option<String> {
    entries
        .iter()
        .rev()
        .filter(|e| rec_type(e) == Some("response_item") && payload_type(e) == Some("message"))
        .filter_map(payload)
        .filter(|p| p.get("role").and_then(|r| r.as_str()) == Some("assistant"))
        .find_map(|p| full_text(p.get("content")?, &["output_text", "text"]))
}

/// Cumulative `(input, output)` token totals — read straight off the most
/// recent `token_count` event's `total_token_usage`, which codex maintains as
/// a running sum for the whole session.
pub fn extract_token_totals(entries: &[Value]) -> (u64, u64) {
    entries
        .iter()
        .rev()
        .find(|e| rec_type(e) == Some("event_msg") && payload_type(e) == Some("token_count"))
        .and_then(|e| payload(e)?.get("info")?.get("total_token_usage").cloned())
        .map(|u| {
            let f = |k: &str| u.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            (f("input_tokens"), f("output_tokens"))
        })
        .unwrap_or((0, 0))
}

pub fn extract_messages(entries: &[Value], count: usize) -> Vec<ConversationMessage> {
    let mut out = Vec::new();
    for entry in entries {
        if rec_type(entry) != Some("response_item") || payload_type(entry) != Some("message") {
            continue;
        }
        let Some(p) = payload(entry) else { continue };
        let role = match p.get("role").and_then(|r| r.as_str()) {
            Some("user") => "user",
            Some("assistant") => "assistant",
            // developer/system messages are not part of the visible dialogue.
            _ => continue,
        };
        let preview = message_text(p, 200).unwrap_or_else(|| NO_TEXT_CONTENT.to_string());
        out.push(ConversationMessage {
            role: role.to_string(),
            content_preview: preview,
            timestamp: record_timestamp(entry).unwrap_or(0),
            model: None,
            stop_reason: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        });
    }
    if out.len() > count {
        out.split_off(out.len() - count)
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::codex::test_util::*;
    use serde_json::json;

    #[test]
    fn last_assistant_message_skips_user_turns() {
        let entries = vec![
            assistant_item("old"),
            assistant_item("line one\nline two"),
            user_msg("thanks"),
        ];
        assert_eq!(
            extract_last_assistant_message(&entries).as_deref(),
            Some("line one\nline two")
        );
    }

    #[test]
    fn token_totals_from_cumulative() {
        let e = vec![event(
            "token_count",
            json!({"info": {"total_token_usage": {"input_tokens": 900, "output_tokens": 120}}}),
        )];
        assert_eq!(extract_token_totals(&e), (900, 120));
    }

    // --- user messages -------------------------------------------------

    #[test]
    fn last_user_message_prefers_event() {
        let e = vec![
            user_msg("first"),
            assistant_item("reply"),
            user_msg("second"),
        ];
        assert_eq!(extract_last_user_message(&e).as_deref(), Some("second"));
        assert_eq!(extract_first_user_message(&e).as_deref(), Some("first"));
    }

    #[test]
    fn user_message_from_response_item() {
        let e = vec![item(
            "message",
            json!({"role": "user", "content": [{"type": "input_text", "text": "hi there"}]}),
        )];
        assert_eq!(extract_last_user_message(&e).as_deref(), Some("hi there"));
    }

    #[test]
    fn environment_context_is_not_a_visible_user_message() {
        let e = vec![
            user_msg("initial request"),
            assistant_item("working"),
            user_msg("<environment_context>\nrepo metadata\n</environment_context>"),
        ];
        assert_eq!(
            extract_last_user_message(&e).as_deref(),
            Some("initial request")
        );
        assert_eq!(
            extract_first_user_message(&e).as_deref(),
            Some("initial request")
        );
    }

    #[test]
    fn environment_context_only_does_not_name_a_session() {
        let e = vec![user_msg(
            "<environment_context>\nrepo metadata\n</environment_context>",
        )];
        assert_eq!(extract_last_user_message(&e), None);
        assert_eq!(extract_first_user_message(&e), None);
    }

    // --- extract_messages ----------------------------------------------

    #[test]
    fn extract_messages_maps_user_and_assistant() {
        let e = vec![
            item(
                "message",
                json!({"role": "user", "content": [{"type": "input_text", "text": "hello"}]}),
            ),
            item("reasoning", json!({"summary": "thinking"})),
            assistant_item("hi back"),
        ];
        let msgs = extract_messages(&e, 10);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content_preview, "hello");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].content_preview, "hi back");
    }
}
