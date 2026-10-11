//! Sessions tab: every session grouped under its project, in one of three
//! layouts `v` cycles through: a narrow list beside the selected session's
//! live pane, a full-width compact list, or a card grid. Held sessions keep
//! their project groups but sit below the rest, under an On hold header.
//!
//! - `split`: the sidebar-and-dock layout
//! - `grid`: the card grid layout
//! - `card`: one card's border, title and task badge
//! - `card_body`: the rows inside a card
//! - `list`: the one-row-per-session layout
//! - `detail`: the per-session detail popup

use crate::app::App;
use crate::models::{first_line_truncated, ProjectGroup, SessionInfo, SessionState, TaskBadge};
use crate::ui::common::{
    context_window_size, state_color, task_color, Dot, COLD_CACHE_ICON, CURSOR, STATE_DOT,
};
use crate::ui::palette::{CONTEXT_GRAY, ICE_BLUE, MUTED_TEXT, SEP_GRAY};
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
mod split;

pub(crate) use detail::render_popup;

const GROUP_HEADER_HEIGHT: u16 = 1;
const GROUP_GAP: u16 = 1;
const HOLD_HEADER_HEIGHT: u16 = 1;

/// What the user has done to a session row or card, as opposed to what the
/// session is doing: the cursor is on it, `h` marked it for a handoff,
/// Space put it on hold, or `r` is renaming it — `draft` is the name typed
/// so far, drawn where the title goes.
#[derive(Clone, Copy, Debug, Default)]
struct CardMarks<'a> {
    selected: bool,
    handoff: bool,
    held: bool,
    draft: Option<&'a str>,
}

impl<'a> CardMarks<'a> {
    fn of(app: &'a App, gi: usize, si: usize, session: &SessionInfo) -> Self {
        Self {
            selected: gi == app.sessions.sel_group && si == app.sessions.sel_in_group,
            handoff: app.sessions.hands_off(&session.session_id),
            held: app.sessions.holds.contains(&session.session_id),
            draft: app.rename_draft(session),
        }
    }
}

/// Sessions-tab body: dispatch to whichever layout is active (see
/// [`crate::app::SessionsLayout`]).
pub(crate) fn render_sessions_body(frame: &mut Frame, area: Rect, app: &mut App) {
    match app.sessions.layout {
        crate::app::SessionsLayout::Split => split::render_split(frame, area, app),
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

/// Rows the On hold header takes above group `gi`: only the first held
/// group carries it.
fn hold_header_above(groups: &[ProjectGroup], gi: usize) -> u16 {
    let opens_hold = groups[gi].held && (gi == 0 || !groups[gi - 1].held);
    if opens_hold {
        HOLD_HEADER_HEIGHT
    } else {
        0
    }
}

/// Group `gi`'s header at content row `g_y`, and the On hold header above
/// it when the group opens that section. Rows scrolled out of `area` are
/// skipped. Shared by the grid and list layouts.
fn render_headers(
    frame: &mut Frame,
    area: Rect,
    groups: &[ProjectGroup],
    gi: usize,
    g_y: u16,
    scroll: u16,
) {
    let on_screen = |y: u16| {
        let sy = y as i32 - scroll as i32;
        (sy >= 0 && sy < area.height as i32)
            .then(|| Rect::new(area.x, area.y + sy as u16, area.width, 1))
    };
    let above = hold_header_above(groups, gi);
    if let Some(row) = (above > 0).then(|| on_screen(g_y - above)).flatten() {
        render_hold_header(frame, row, groups);
    }
    if let Some(row) = on_screen(g_y) {
        render_group_header(frame, row, &groups[gi]);
    }
}

/// One-row On hold header: the held sessions' dot, the held count, then a
/// rule to the right edge.
fn render_hold_header(frame: &mut Frame, area: Rect, groups: &[ProjectGroup]) {
    let held: usize = groups
        .iter()
        .filter(|g| g.held)
        .map(|g| g.sessions.len())
        .sum();
    let dot = format!(" {STATE_DOT}");
    let title = " On hold ";
    let count = format!(" {held} held ");
    let used = dot.chars().count() + title.chars().count() + count.chars().count();
    let rule = "─".repeat((area.width as usize).saturating_sub(used));
    let line = Line::from(vec![
        // The colour every held session's dot takes, see `dot_color`.
        Span::styled(dot, Style::default().fg(state_color(&SessionState::Idle))),
        Span::styled(
            title,
            Style::default().fg(MUTED_TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(count, Style::default().fg(Color::DarkGray)),
        Span::styled(rule, Style::default().fg(SEP_GRAY)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// One-row project group header: name, session count, attention count, cwd.
fn render_group_header(frame: &mut Frame, area: Rect, group: &ProjectGroup) {
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
            format!("  {STATE_DOT} {attn}"),
            Style::default().fg(state_color(&SessionState::WaitingForInput)),
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

/// The state dot's colour, animated: a working session blinks. A held
/// session reads as idle: it is parked, whatever its state.
fn dot_color(state: &SessionState, marks: CardMarks, now: u64) -> Color {
    match marks.held {
        true => Dot::Resting.color(now),
        false => Dot::of(state).color(now),
    }
}

/// A one-row title and its style, fitted to `width`: the Haiku title,
/// always white — the dot beside it is the only thing the state colours.
/// Without one, the last user message stands in, as on the card body. A
/// rename in progress shows its draft instead. Bold is the cursor's: only
/// the selected session's name carries it.
fn row_title(session: &SessionInfo, marks: CardMarks, width: usize) -> (String, Style) {
    let (text, style) = match (marks.draft, session.title.as_deref()) {
        (Some(draft), _) => (draft_text(draft, width), Style::default().fg(Color::White)),
        (None, Some(t)) if !t.is_empty() => (
            first_line_truncated(t, width),
            Style::default().fg(Color::White),
        ),
        _ if session.titling => ("✎ …".to_string(), Style::default().fg(MUTED_TEXT)),
        _ => {
            let msg = session
                .last_user_message
                .as_ref()
                .or(session.summary.as_ref())
                .map(|m| m.replace('\n', " "))
                .unwrap_or_default();
            (
                first_line_truncated(&msg, width),
                Style::default().fg(CONTEXT_GRAY),
            )
        }
    };
    if marks.selected {
        (text, style.add_modifier(Modifier::BOLD))
    } else {
        (text, style)
    }
}

/// The name being typed with the cursor after it, fitted to `width` from
/// the front: a name longer than its slot scrolls, so the cursor stays in
/// sight.
fn draft_text(draft: &str, width: usize) -> String {
    let overflow = (draft.chars().count() + 1).saturating_sub(width);
    if overflow == 0 {
        return format!("{draft}{CURSOR}");
    }
    let tail: String = draft.chars().skip(overflow + 1).collect();
    format!("…{tail}{CURSOR}")
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
            .draw(|f| {
                super::card::render_card(f, f.area(), s, None, super::CardMarks::default(), NOW)
            })
            .expect("render");
        terminal.backend().buffer().clone()
    }

    pub(super) fn row(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buf.area().width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::draft_text;

    #[test]
    fn a_long_draft_scrolls_to_keep_the_cursor_in_sight() {
        assert_eq!(draft_text("abc", 5), "abc▎");
        assert_eq!(draft_text("abcd", 5), "abcd▎");
        assert_eq!(draft_text("abcdef", 5), "…def▎");
    }
}
