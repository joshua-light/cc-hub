use crate::conversation;
use crate::models::ConversationMessage;
use std::path::PathBuf;

pub struct LiveView {
    path: PathBuf,
    file_len: u64,
    pub messages: Vec<ConversationMessage>,
    pub scroll: u16,
    pub auto_scroll: bool,
    pub total_content_lines: u16,
    /// Message index to highlight, resolved once from the peak timestamp at
    /// construction so the renderer does a cheap pointer compare.
    pub highlight_msg_idx: Option<usize>,
    /// Consumed on the first frame that places the highlight — `Option::take`d
    /// so later polls don't fight the user's manual scroll.
    pub scroll_to_highlight: Option<()>,
    pub review_mode: bool,
}

impl LiveView {
    pub fn new(jsonl_path: PathBuf) -> Self {
        let file_len = std::fs::metadata(&jsonl_path)
            .map(|m| m.len())
            .unwrap_or(0);

        let entries = conversation::read_jsonl_tail(&jsonl_path, 128 * 1024);
        let messages = conversation::extract_messages(&entries, 100);

        Self {
            path: jsonl_path,
            file_len,
            messages,
            scroll: 0,
            auto_scroll: true,
            total_content_lines: 0,
            highlight_msg_idx: None,
            scroll_to_highlight: None,
            review_mode: false,
        }
    }

    /// Open a session for post-mortem review. Reads the full JSONL and every
    /// message so `highlight_ts` (if any) resolves to a concrete index
    /// up-front, and pins `auto_scroll` off so the renderer can jump to the
    /// peak instead of snapping to the bottom.
    pub fn review(jsonl_path: PathBuf, highlight_ts: Option<u64>) -> Self {
        let file_len = std::fs::metadata(&jsonl_path)
            .map(|m| m.len())
            .unwrap_or(0);
        let entries = conversation::read_jsonl_all(&jsonl_path);
        let messages = conversation::extract_messages(&entries, usize::MAX);
        // Closest assistant message by timestamp (tolerates drift between the
        // metrics snapshot and the on-disk transcript).
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
            file_len,
            messages,
            scroll: 0,
            auto_scroll: false,
            total_content_lines: 0,
            highlight_msg_idx,
            scroll_to_highlight: highlight_msg_idx.map(|_| ()),
            review_mode: true,
        }
    }

    /// Check if the JSONL file has grown and parse new messages.
    /// Returns true if messages changed.
    pub fn poll(&mut self) -> bool {
        if self.review_mode {
            return false;
        }
        let new_len = match std::fs::metadata(&self.path) {
            Ok(m) => m.len(),
            Err(_) => return false,
        };

        if new_len == self.file_len {
            return false;
        }
        self.file_len = new_len;

        let entries = conversation::read_jsonl_tail(&self.path, 128 * 1024);
        let messages = conversation::extract_messages(&entries, 100);

        if messages.len() == self.messages.len() {
            return false;
        }

        self.messages = messages;
        true
    }

    pub fn scroll_up(&mut self) {
        self.auto_scroll = false;
        self.scroll = self.scroll.saturating_sub(3);
    }

    pub fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_add(3);
        // Re-enable auto-scroll if we're near the bottom
        if self.scroll + 5 >= self.total_content_lines {
            self.auto_scroll = true;
        }
    }

    pub fn scroll_bottom(&mut self) {
        self.auto_scroll = true;
        // Actual scroll position set during render when we know the viewport height
    }
}
