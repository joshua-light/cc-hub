use crate::agent::AgentKind;
use crate::conversation;
use crate::models::ConversationMessage;
use crate::pi_conversation;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Minimum spacing between filesystem polls while a LiveTail is open. The
/// render loop wakes every ~50ms, but re-`stat`ing the JSONL (and, when it
/// grew, re-reading + re-parsing up to 128KB) that often is pure overhead on
/// the draw thread for a transcript that appends at most a few lines a
/// second. Polling at ~4Hz keeps the tail feeling live without the churn.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

pub struct LiveView {
    path: PathBuf,
    agent_kind: AgentKind,
    file_len: u64,
    last_poll: Option<Instant>,
    pub messages: Vec<ConversationMessage>,
    pub scroll: u16,
    pub auto_scroll: bool,
    pub total_content_lines: u16,
    pub highlight_msg_idx: Option<usize>,
    pub scroll_to_highlight: Option<()>,
    pub review_mode: bool,
}

impl LiveView {
    pub fn new(jsonl_path: PathBuf, agent_kind: AgentKind) -> Self {
        let file_len = std::fs::metadata(&jsonl_path).map(|m| m.len()).unwrap_or(0);

        let entries = conversation::read_jsonl_tail(&jsonl_path, 128 * 1024);
        let messages = extract_messages(agent_kind, &entries, 100);

        Self {
            path: jsonl_path,
            agent_kind,
            file_len,
            last_poll: None,
            messages,
            scroll: 0,
            auto_scroll: true,
            total_content_lines: 0,
            highlight_msg_idx: None,
            scroll_to_highlight: None,
            review_mode: false,
        }
    }

    pub fn review(jsonl_path: PathBuf, agent_kind: AgentKind, highlight_ts: Option<u64>) -> Self {
        let file_len = std::fs::metadata(&jsonl_path).map(|m| m.len()).unwrap_or(0);
        let entries = conversation::read_jsonl_all(&jsonl_path);
        let messages = extract_messages(agent_kind, &entries, usize::MAX);
        let highlight_msg_idx = highlight_ts.and_then(|ts| {
            messages
                .iter()
                .enumerate()
                .filter(|(_, m)| m.role == "assistant" && m.timestamp > 0)
                .min_by_key(|(_, m)| m.timestamp.abs_diff(ts))
                .map(|(i, _)| i)
        });
        Self {
            path: jsonl_path,
            agent_kind,
            file_len,
            last_poll: None,
            messages,
            scroll: 0,
            auto_scroll: false,
            total_content_lines: 0,
            highlight_msg_idx,
            scroll_to_highlight: highlight_msg_idx.map(|_| ()),
            review_mode: true,
        }
    }

    pub fn poll(&mut self) -> bool {
        if self.review_mode {
            return false;
        }
        // Throttle to ~4Hz: the draw loop calls this every frame, but a
        // `stat` + possible 128KB re-parse per frame is wasteful for a
        // transcript that grows slowly. Skipping a poll just defers picking
        // up new lines by at most `POLL_INTERVAL`, which is imperceptible.
        let now = Instant::now();
        if let Some(last) = self.last_poll {
            if now.duration_since(last) < POLL_INTERVAL {
                return false;
            }
        }
        self.last_poll = Some(now);

        let new_len = match std::fs::metadata(&self.path) {
            Ok(m) => m.len(),
            Err(_) => return false,
        };

        if new_len == self.file_len {
            return false;
        }
        self.file_len = new_len;

        let entries = conversation::read_jsonl_tail(&self.path, 128 * 1024);
        let messages = extract_messages(self.agent_kind, &entries, 100);

        // The file grew (checked above), so re-parse and adopt the result.
        // Comparing `messages.len()` was wrong: the tail is capped at 100
        // messages, so once a busy session stays pinned at that cap the length
        // never changes even as content scrolls in — the old code parsed the
        // fresh tail then threw it away, freezing the tail forever.
        self.messages = messages;
        true
    }

    pub fn scroll_up(&mut self) {
        self.auto_scroll = false;
        self.scroll = self.scroll.saturating_sub(3);
    }

    pub fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_add(3);
        if self.scroll + 5 >= self.total_content_lines {
            self.auto_scroll = true;
        }
    }

    pub fn scroll_bottom(&mut self) {
        self.auto_scroll = true;
    }
}

fn extract_messages(
    agent_kind: AgentKind,
    entries: &[serde_json::Value],
    count: usize,
) -> Vec<ConversationMessage> {
    match agent_kind {
        AgentKind::Claude => conversation::extract_messages(entries, count),
        AgentKind::Pi => pi_conversation::extract_messages(entries, count),
        AgentKind::Codex => crate::codex_conversation::extract_messages(entries, count),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const ASSISTANT_LINE: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#;

    fn write_lines(path: &std::path::Path, n: usize) {
        let mut f = std::fs::File::create(path).expect("create");
        for _ in 0..n {
            writeln!(f, "{}", ASSISTANT_LINE).expect("write");
        }
        f.flush().expect("flush");
    }

    fn append_line(path: &std::path::Path) {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .expect("open append");
        writeln!(f, "{}", ASSISTANT_LINE).expect("append");
        f.flush().expect("flush");
    }

    #[test]
    fn poll_throttles_back_to_back_calls() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.jsonl");
        write_lines(&path, 1);

        let mut lv = LiveView::new(path.clone(), AgentKind::Claude);
        assert_eq!(lv.messages.len(), 1);

        // First poll consumes the throttle gate (no growth → false).
        assert!(!lv.poll());

        // Even though the file just grew, a poll within POLL_INTERVAL is
        // skipped and the new line is not yet reflected.
        append_line(&path);
        assert!(!lv.poll());
        assert_eq!(lv.messages.len(), 1);
    }

    #[test]
    fn poll_picks_up_growth_after_interval() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.jsonl");
        write_lines(&path, 1);

        let mut lv = LiveView::new(path.clone(), AgentKind::Claude);
        assert!(!lv.poll());

        // Simulate the throttle window having elapsed, then grow the file.
        lv.last_poll = Some(Instant::now() - POLL_INTERVAL - Duration::from_millis(1));
        append_line(&path);
        assert!(lv.poll());
        assert_eq!(lv.messages.len(), 2);
    }

    #[test]
    fn review_mode_never_polls() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.jsonl");
        write_lines(&path, 1);

        let mut lv = LiveView::review(path.clone(), AgentKind::Claude, None);
        append_line(&path);
        // Backdate so the throttle would otherwise allow a poll.
        lv.last_poll = Some(Instant::now() - POLL_INTERVAL - Duration::from_millis(1));
        assert!(!lv.poll());
        assert_eq!(lv.messages.len(), 1);
    }
}
