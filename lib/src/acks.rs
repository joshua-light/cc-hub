use std::collections::{HashMap, HashSet};

/// Tracks user acknowledgements that force a session to display as Idle.
///
/// An ack is stamped with the session's `last_activity` at press time. While the
/// live watermark still matches, the session is treated as Idle regardless of
/// its real state (WaitingForInput or Processing). Any new activity advances
/// the watermark and auto-clears the ack.
#[derive(Default)]
pub struct Acks {
    entries: HashMap<String, Option<u64>>,
}

impl Acks {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn ack(&mut self, session_id: &str, watermark: Option<u64>) {
        self.entries.insert(session_id.to_string(), watermark);
    }

    /// Returns true if the ack still applies, false otherwise.
    /// Automatically removes stale entries whose watermark no longer matches.
    pub fn is_acked(&mut self, session_id: &str, current: Option<u64>) -> bool {
        match self.entries.get(session_id) {
            Some(stamped) if *stamped == current => true,
            Some(_) => {
                self.entries.remove(session_id);
                false
            }
            None => false,
        }
    }

    /// Drop acks for session ids that no longer appear.
    pub fn retain_existing(&mut self, live_ids: &HashSet<&str>) {
        self.entries.retain(|id, _| live_ids.contains(id.as_str()));
    }
}
