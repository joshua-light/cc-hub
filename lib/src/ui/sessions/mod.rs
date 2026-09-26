//! Sessions tab: every session grouped under its project, as a card grid
//! or, toggled with `v`, a compact list.
//!
//! - `grid`: the card grid layout
//! - `card`: one card's border, title and task badge
//! - `card_body`: the rows inside a card
//! - `list`: the one-row-per-session layout
//! - `detail`: the per-session detail popup

use crate::app::App;
use crate::ui::palette::SEP_GRAY;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

mod card;
mod card_body;
mod detail;
mod grid;
mod list;

pub(crate) use detail::render_popup;

const GROUP_HEADER_HEIGHT: u16 = 1;
const GROUP_GAP: u16 = 1;

/// Sessions-tab body: dispatch to whichever layout is active. The card grid
/// is the default; the compact list is the `v`-toggled experiment (see
/// [`crate::app::SessionsLayout`]).
pub(crate) fn render_sessions_body(frame: &mut Frame, area: Rect, app: &mut App) {
    match app.sessions.layout {
        crate::app::SessionsLayout::Grid => grid::render_grid(frame, area, app),
        crate::app::SessionsLayout::List => list::render_list(frame, area, app),
    }
}

/// The empty-state hint, shared by both layouts.
fn render_no_sessions(frame: &mut Frame, area: Rect) {
    let empty = Paragraph::new("No sessions found. Start an agent session to see it here.")
        .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(empty, area);
}

/// One-row project group header: name, session count, attention count, cwd.
/// Shared by the grid and list layouts.
fn render_group_header(frame: &mut Frame, area: Rect, group: &crate::models::ProjectGroup) {
    let total = group.sessions.len();
    let attn = group
        .sessions
        .iter()
        .filter(|s| s.needs_attention())
        .count();

    let mut spans = vec![Span::styled(
        format!(" 󰉋 {} ", group.name),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )];
    spans.push(Span::styled(
        format!(" {} sessions", total),
        Style::default().fg(Color::DarkGray),
    ));
    if attn > 0 {
        spans.push(Span::styled(
            format!("  󰂞 {}", attn),
            Style::default().fg(Color::Yellow),
        ));
    }
    // Show cwd path dimmed after the counts
    spans.push(Span::styled(
        format!("  {}", group.cwd),
        Style::default().fg(SEP_GRAY),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod fixtures {
    use crate::agent::AgentKind;
    use crate::models::{SessionInfo, SessionState};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Fixed render clock; sessions stamp ages relative to this.
    pub(super) const NOW: u64 = 10_000_000_000;

    pub(super) fn fake_session() -> SessionInfo {
        SessionInfo {
            agent_id: "claude".into(),
            agent_kind: AgentKind::Claude,
            pid: 4242,
            session_id: "abcd1234efgh".into(),
            cwd: "/tmp/p".into(),
            project_name: "p".into(),
            started_at: NOW - 3_600_000,
            last_activity: Some(NOW - 720_000), // 12m ago
            state: SessionState::Idle,
            last_user_message: None,
            summary: None,
            title: None,
            titling: false,
            model: Some("claude-opus-4-8".into()),
            git_branch: Some("main".into()),
            version: None,
            jsonl_path: None,
            tmux_session: None,
            current_tool: None,
            is_thinking: false,
            context_tokens: None,
            tool_uses_count: 0,
        }
    }

    pub(super) fn render(s: &SessionInfo, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| super::card::render_card(f, f.area(), s, None, false, NOW))
            .expect("render");
        terminal.backend().buffer().clone()
    }

    pub(super) fn row(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buf.area().width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }
}
