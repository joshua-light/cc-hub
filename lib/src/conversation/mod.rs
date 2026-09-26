//! Agent transcript parsing: reading session logs, deriving session state,
//! extracting messages/metadata, and rendering content previews.
//!
//! Claude parsing lives in focused submodules, all re-exported flat at
//! `conversation::*`:
//! - [`io`] — JSONL reading, the grow-the-tail loop every dialect shares, and
//!   streaming block counters.
//! - [`cache`] — mtime-keyed memoization of derived state and summaries.
//! - [`state`] — entry classification and the session-state machine.
//! - [`messages`] — message and metadata extraction.
//! - [`render`] — content-preview and tool-display rendering.
//!
//! Other dialects and helpers stay behind their module names, because their
//! items would clash with the flat Claude API:
//! - [`pi`] — Pi transcript parsing.
//! - [`codex`] — Codex rollout parsing.
//! - [`tool_count`] — incremental tool-use counts across every dialect.
//! - [`classify`] — the session-state machine every dialect adapts to.

use crate::agent::AgentKind;
use crate::models::ConversationMessage;
use serde_json::Value;

mod cache;
pub(crate) mod classify;
pub mod codex;
mod io;
mod messages;
pub mod pi;
mod render;
pub(crate) mod state;
pub mod tool_count;

#[cfg(test)]
mod test_util;

pub use cache::{derive_state_cached, first_user_message_cached, retain_cached, StateDerivation};
pub use io::{
    count_blocks_in_reader, count_blocks_of_type, count_tool_uses_in_reader, read_jsonl_all,
    read_jsonl_head, read_jsonl_tail, read_jsonl_tail_for_state,
};
pub(crate) use messages::{extract_cwd, extract_started_at};
pub use messages::{
    extract_first_user_message, extract_last_activity, extract_last_user_message, extract_messages,
    extract_metadata, extract_token_totals, parse_timestamp_ms, AUTOMATION_ROLE,
};
pub(crate) use render::{NO_CONTENT, NO_TEXT_CONTENT, THINKING_MARKER, TOOL_MARKER_PREFIX};
pub use state::{
    extract_context_tokens, extract_current_tool, extract_state, is_currently_thinking, CurrentTool,
};

/// [`extract_messages`] in `kind`'s transcript dialect.
pub(crate) fn extract_messages_for(
    kind: AgentKind,
    entries: &[Value],
    count: usize,
) -> Vec<ConversationMessage> {
    match kind {
        AgentKind::Claude => extract_messages(entries, count),
        AgentKind::Pi => pi::extract_messages(entries, count),
        AgentKind::Codex => codex::extract_messages(entries, count),
    }
}

/// [`extract_token_totals`] in `kind`'s transcript dialect.
pub(crate) fn extract_token_totals_for(kind: AgentKind, entries: &[Value]) -> (u64, u64) {
    match kind {
        AgentKind::Claude => extract_token_totals(entries),
        AgentKind::Pi => pi::extract_token_totals(entries),
        AgentKind::Codex => codex::extract_token_totals(entries),
    }
}
