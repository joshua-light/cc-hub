//! Claude JSONL transcript parsing: reading session logs, deriving session
//! state, extracting messages/metadata, and rendering content previews.
//!
//! Split into focused submodules, all re-exported flat at `conversation::*`:
//! - [`io`] — JSONL reading and streaming block counters.
//! - [`cache`] — mtime-keyed memoization of derived state and summaries.
//! - [`state`] — entry classification and the session-state machine.
//! - [`explain`] — instrumented mirror of the state machine for the debug popup.
//! - [`messages`] — message and metadata extraction.
//! - [`render`] — content-preview and tool-display rendering.

mod cache;
mod explain;
mod io;
mod messages;
mod render;
mod state;

#[cfg(test)]
mod test_util;

pub use cache::{derive_state_cached, first_user_message_cached, retain_cached, StateDerivation};
pub use explain::{explain_state, EntrySummary, ExplanationStep, StateExplanation, Verdict};
pub use io::{
    count_blocks_in_reader, count_blocks_of_type, count_tool_uses, count_tool_uses_in_reader,
    read_jsonl_all, read_jsonl_head, read_jsonl_tail, read_jsonl_tail_for_state,
};
pub use messages::{
    extract_first_user_message, extract_last_activity, extract_last_user_message, extract_messages,
    extract_metadata, extract_token_totals, parse_timestamp_ms,
};
pub(crate) use render::{NO_CONTENT, NO_TEXT_CONTENT, THINKING_MARKER, TOOL_MARKER_PREFIX};
pub use state::{
    extract_context_tokens, extract_current_tool, extract_state, is_currently_thinking, CurrentTool,
};
