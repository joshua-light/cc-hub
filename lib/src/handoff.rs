//! Handoff (`h` on the Sessions tab): continue a session in a fresh one
//! instead of compacting it.
//!
//! `h` marks a session and takes its last reply. The next session the user
//! starts — `n`, `N`, an agent hotkey, a folder pick — opens with that reply
//! typed into its input as a [`Handoff::draft`], unsent: the user writes what
//! to do with it and presses Enter. A named source also names its successor:
//! `Plan` hands off to `Plan (2)`, `Plan (2)` to `Plan (3)`.

use crate::conversation;
use crate::models::SessionInfo;

/// How much of the transcript's end is read for the last reply. A reply sits
/// at the end by definition; the slack covers tool results written after it.
const TAIL_BYTES: u64 = 2 * 1024 * 1024;

/// A marked session and the prompt it hands to the next fresh one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handoff {
    pub session_id: String,
    /// The next session's name, the source's [`sequel`]. `None` for an
    /// unnamed source: the next one is named the usual way.
    pub title: Option<String>,
    /// The source's last reply wrapped in `<context>`, then a blank line
    /// where the user's own words go. Read when the mark is made, so what
    /// arrives is what was marked even if the source keeps working.
    pub draft: String,
}

impl Handoff {
    /// `None` when `session` has no transcript yet, or it holds no reply.
    pub fn of(session: &SessionInfo) -> Option<Self> {
        let entries = conversation::read_jsonl_tail(session.jsonl_path.as_ref()?, TAIL_BYTES);
        let reply =
            conversation::extract_last_assistant_message_for(session.agent_kind()?, &entries)?;
        Some(Self {
            session_id: session.session_id.clone(),
            title: session
                .title
                .as_deref()
                .filter(|t| !t.is_empty())
                .map(sequel),
            draft: format!("<context>\n{reply}\n</context>\n\n"),
        })
    }
}

/// `title` numbered one further: a trailing ` (n)` counts up, and a title
/// without one is the first, so its sequel is ` (2)`.
fn sequel(title: &str) -> String {
    let numbered = title
        .strip_suffix(')')
        .and_then(|t| t.rsplit_once(" ("))
        .and_then(|(name, n)| Some((name, n.parse::<u32>().ok()?.checked_add(1)?)));
    match numbered {
        Some((name, next)) => format!("{name} ({next})"),
        None => format!("{title} (2)"),
    }
}

#[cfg(test)]
mod tests {
    use super::sequel;

    #[test]
    fn a_first_session_is_followed_by_its_second() {
        assert_eq!(sequel("Plan"), "Plan (2)");
    }

    #[test]
    fn a_numbered_session_counts_up() {
        assert_eq!(sequel("Plan (2)"), "Plan (3)");
        assert_eq!(sequel("Plan (9)"), "Plan (10)");
    }

    #[test]
    fn parentheses_that_are_not_a_number_are_part_of_the_name() {
        assert_eq!(sequel("Plan (draft)"), "Plan (draft) (2)");
        assert_eq!(sequel("(3)"), "(3) (2)");
    }
}
