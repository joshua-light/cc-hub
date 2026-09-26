//! Sessions tab: every session grouped under its project, as a card grid
//! or, toggled with `v`, a compact list.
//!
//! - `grid`: the card grid layout
//! - `card`: one card's border, title and task badge
//! - `card_body`: the rows inside a card
//! - `list`: the one-row-per-session layout
//! - `detail`: the per-session detail popup

use crate::app::App;
use crate::models::{SessionInfo, SessionState, TaskBadge};
use crate::ui::common::{
    context_window_size, spinner_frame, starting_frame, state_indicator, task_color,
    COLD_CACHE_ICON,
};
use crate::ui::palette::{ICE_BLUE, SEP_GRAY};
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
    spans.push(Span::styled(
        format!("  {}", group.cwd),
        Style::default().fg(SEP_GRAY),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Scroll so the selected item (content rows `item_top..item_bottom`) is on
/// screen, with its group header too when both fit in `view_h` rows.
fn keep_in_view(scroll: &mut u16, group_top: u16, item_top: u16, item_bottom: u16, view_h: u16) {
    if item_bottom.saturating_sub(group_top) <= view_h {
        if group_top < *scroll {
            *scroll = group_top;
        } else if item_bottom > *scroll + view_h {
            *scroll = item_bottom.saturating_sub(view_h);
        }
    } else if item_top < *scroll {
        *scroll = item_top;
    } else if item_bottom > *scroll + view_h {
        *scroll = item_bottom.saturating_sub(view_h);
    }
}

/// State glyph and colour, animated: Processing spins, Starting orbits.
fn animated_indicator(state: &SessionState, now: u64) -> (&'static str, Color) {
    let (indicator, color) = state_indicator(state);
    let indicator = match state {
        SessionState::Processing => spinner_frame(now),
        SessionState::Starting => starting_frame(now),
        _ => indicator,
    };
    (indicator, color)
}

/// `[Codex] ` ahead of a non-Claude session's title. Claude is the ~99%
/// default, so labelling it would be noise.
fn agent_prefix(session: &SessionInfo) -> String {
    if session.agent_id == "claude" {
        String::new()
    } else {
        format!("[{}] ", session.agent_badge())
    }
}

/// Icon and colour of the last-activity clock. Past the prompt-cache TTL the
/// clock becomes the ice-blue snowflake: restarting beats resuming.
fn activity_clock(session: &SessionInfo, now: u64) -> (&'static str, Color) {
    if session.cache_cold(now) {
        (COLD_CACHE_ICON, ICE_BLUE)
    } else {
        ("󰔟", Color::DarkGray)
    }
}

/// Context-window use as (percent capped at 999, percent clamped to 100 for
/// the colour ramp and bar); `None` without a token count.
fn context_pct(session: &SessionInfo) -> Option<(f64, u8)> {
    let ctx = session.context_tokens?;
    let window = context_window_size(session.model.as_deref().unwrap_or(""));
    let pct = ((ctx as f64 / window as f64) * 100.0).min(999.0);
    Some((pct, (pct as u64).min(100) as u8))
}

/// A task badge's identity colour, dimmed to gray once the link is stale.
fn badge_color(badge: &TaskBadge) -> Color {
    if badge.stale {
        Color::DarkGray
    } else {
        task_color(&badge.task_id)
    }
}

#[cfg(test)]
mod fixtures {
    use crate::models::{SessionInfo, SessionState};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Fixed render clock; sessions stamp ages relative to this.
    pub(super) const NOW: u64 = 10_000_000_000;

    pub(super) fn fake_session() -> SessionInfo {
        SessionInfo {
            pid: 4242,
            session_id: "abcd1234efgh".into(),
            cwd: "/tmp/p".into(),
            project_name: "p".into(),
            started_at: NOW - 3_600_000,
            last_activity: Some(NOW - 720_000), // 12m ago
            state: SessionState::Idle,
            model: Some("claude-opus-4-8".into()),
            git_branch: Some("main".into()),
            ..crate::test_util::session_info()
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
