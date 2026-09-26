//! JSON fixtures for the Pi parser tests.

use serde_json::{json, Value};
use std::io::Write as _;

/// Pi user entry: `type=message`, `message.role=user`, string or array content.
pub(super) fn user(content: Value) -> Value {
    json!({"type": "message", "message": {"role": "user", "content": content}})
}

/// Pi assistant entry with the given stopReason and content blocks.
pub(super) fn assistant(stop_reason: &str, content: Value) -> Value {
    json!({"type": "message", "message": {
        "role": "assistant", "stopReason": stop_reason, "content": content
    }})
}

/// Pi `toolCall` content block.
pub(super) fn tool_call(id: &str, name: &str, arguments: Value) -> Value {
    json!({"type": "toolCall", "id": id, "name": name, "arguments": arguments})
}

/// Pi `toolResult` entry resolving the given toolCallId.
pub(super) fn tool_result(id: &str) -> Value {
    json!({"type": "message", "message": {"role": "toolResult", "toolCallId": id}})
}

/// Write JSONL lines to a fresh tempfile and return the handle (keep it
/// alive for the test so the file isn't unlinked).
pub(super) fn write_jsonl(lines: &[&str]) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().expect("create tempfile");
    for line in lines {
        writeln!(f, "{}", line).expect("write line");
    }
    f.flush().expect("flush");
    f
}
