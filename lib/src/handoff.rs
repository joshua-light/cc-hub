//! Handoff (`h` on the Sessions tab): continue a session in a fresh one
//! instead of compacting it.
//!
//! `h` marks a session and takes its last reply. The next session the user
//! starts — `n`, `N`, an agent hotkey, a folder pick — opens with that reply
//! typed into its input as a [`Handoff::draft`], unsent: the user writes what
//! to do with it and presses Enter.

use crate::agent::AgentKind;
use crate::models::SessionInfo;
use crate::{codex_conversation, conversation, pi_conversation};

/// How much of the transcript's end is read for the last reply. A reply sits
/// at the end by definition; the slack covers tool results written after it.
const TAIL_BYTES: u64 = 2 * 1024 * 1024;

/// A marked session and the prompt it hands to the next fresh one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handoff {
    pub session_id: String,
    /// The source's last reply wrapped in `<context>`, then a blank line
    /// where the user's own words go. Read when the mark is made, so what
    /// arrives is what was marked even if the source keeps working.
    pub draft: String,
}

impl Handoff {
    /// `None` when `session` has no transcript yet, or it holds no reply.
    pub fn of(session: &SessionInfo) -> Option<Self> {
        let entries = conversation::read_jsonl_tail(session.jsonl_path.as_ref()?, TAIL_BYTES);
        let reply = match session.agent_kind {
            AgentKind::Claude => conversation::extract_last_assistant_message(&entries),
            AgentKind::Codex => codex_conversation::extract_last_assistant_message(&entries),
            AgentKind::Pi => pi_conversation::extract_last_assistant_message(&entries),
        }?;
        Some(Self {
            session_id: session.session_id.clone(),
            draft: format!("<context>\n{reply}\n</context>\n\n"),
        })
    }
}
