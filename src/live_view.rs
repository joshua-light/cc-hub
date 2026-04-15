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
}

impl LiveView {
    pub fn new(jsonl_path: PathBuf) -> Self {
        let file_len = std::fs::metadata(&jsonl_path)
            .map(|m| m.len())
            .unwrap_or(0);

        // Load initial messages from tail
        let entries = conversation::read_jsonl_tail(&jsonl_path, 128 * 1024);
        let messages = conversation::extract_messages(&entries, 100);

        Self {
            path: jsonl_path,
            file_len,
            messages,
            scroll: 0,
            auto_scroll: true,
            total_content_lines: 0,
        }
    }

    /// Check if the JSONL file has grown and parse new messages.
    /// Returns true if messages changed.
    pub fn poll(&mut self) -> bool {
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
