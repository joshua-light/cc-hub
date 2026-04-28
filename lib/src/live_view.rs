use crate::agent::AgentKind;
use crate::conversation;
use crate::models::ConversationMessage;
use crate::pi_conversation;
use std::path::PathBuf;

pub struct LiveView {
    path: PathBuf,
    agent_kind: AgentKind,
    file_len: u64,
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
    }
}
