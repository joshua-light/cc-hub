use super::card_body::body_lines;
use super::{agent_prefix, animated_indicator, badge_color, CardMarks};
use crate::models::{first_line_truncated, SessionInfo, SessionState, TaskBadge};
use crate::ui::common::{priority_color, state_color, COLD_CACHE_ICON};
use crate::ui::palette::{HANDOFF_BLUE, SEP_GRAY};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

pub(super) fn render_card(
    frame: &mut Frame,
    area: Rect,
    session: &SessionInfo,
    badge: Option<&TaskBadge>,
    marks: CardMarks,
    now: u64,
) {
    let (border_type, border_color) = border(session, marks);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(border_color))
        .title(card_title(session, now));
    if let Some(badge) = badge {
        block = with_task_badge(block, badge, area.width);
    }

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let inner_w = inner.width as usize;
    let inner_h = inner.height as usize;
    let lines = body_lines(session, now, inner_w, inner_h);

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

/// Border weight and colour: white double when selected, thick in the state
/// colour when the session needs you, the state colour while processing.
/// A handoff mark outranks every colour — the weight still says the rest.
fn border(session: &SessionInfo, marks: CardMarks) -> (BorderType, Color) {
    let CardMarks { selected, handoff } = marks;
    let border_color = if handoff {
        HANDOFF_BLUE
    } else if selected {
        Color::White
    } else if session.needs_attention() || session.state == SessionState::Processing {
        // The state colour tells a Question (blue) from a plain wait
        // (yellow); a green Processing frame reads as alive, not ambient.
        state_color(&session.state)
    } else {
        SEP_GRAY
    };

    let border_type = if selected {
        BorderType::Double
    } else if session.needs_attention() {
        // Weight plus the chip title make "needs you" categorical, not a hue
        // shift: Processing shares the coloured border but not the weight.
        BorderType::Thick
    } else if session.state == SessionState::Inactive {
        BorderType::LightDoubleDashed
    } else {
        BorderType::Rounded
    };
    (border_type, border_color)
}

/// The top-border title, the card's main skim surface: agent badge, state
/// glyph, cold-cache mark, then the Haiku title (or `✎ …` while one is
/// being generated). The project name is left out: the group header shows it.
fn card_title(session: &SessionInfo, now: u64) -> Span<'static> {
    let (indicator, ind_color) = animated_indicator(&session.state, now);
    let agent_badge = agent_prefix(session);

    // Every branch keeps a space right after `indicator`: the Nerd Font
    // glyph renders two columns but measures one, so without a trailing cell
    // its second column collides with the border (the bare `󰂞` case).
    //
    // The snowflake marks a cold prompt cache (restarting beats resuming).
    // Here it takes the title's colour; the ice-blue one is the footer clock.
    let cold_mark = if session.cache_cold(now) {
        format!("{} ", COLD_CACHE_ICON)
    } else {
        String::new()
    };
    let title = match session.title.as_deref() {
        Some(t) if !t.is_empty() => {
            format!("{}{} {}{}", agent_badge, indicator, cold_mark, t)
        }
        _ if session.titling => format!("{}{} {}✎ …", agent_badge, indicator, cold_mark),
        _ => format!("{}{} {}", agent_badge, indicator, cold_mark),
    };
    // Attention cards get a filled chip (black on the state colour).
    // Background fill is reserved for "needs you", so it can't be mistaken
    // for the coloured but ambient Processing border.
    let (title, title_style) = if session.needs_attention() {
        let chip = if title.ends_with(' ') {
            format!(" {}", title)
        } else {
            format!(" {} ", title)
        };
        (
            chip,
            Style::default()
                .fg(Color::Black)
                .bg(ind_color)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            title,
            Style::default().fg(ind_color).add_modifier(Modifier::BOLD),
        )
    };
    Span::styled(title, title_style)
}

/// Task link mark (`L`): the task title on the bottom border in the task's
/// identity colour, so every card of one task carries the same mark. A
/// stale link dims to gray. The priority sits bottom-right in the board's
/// priority hue, as text rather than a chip (fill means "needs you"), and
/// the title's budget shrinks so the two never collide.
fn with_task_badge<'a>(mut block: Block<'a>, badge: &TaskBadge, width: u16) -> Block<'a> {
    let color = badge_color(badge);
    let prio_w = badge.priority.map_or(0, |p| p.label().chars().count() + 2);
    let label = format!(
        " 󰓹 {} ",
        first_line_truncated(
            &badge.title,
            (width as usize).saturating_sub(7 + prio_w).max(4)
        )
    );
    block = block.title_bottom(Line::from(Span::styled(
        label,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )));
    if let Some(priority) = badge.priority {
        let prio_color = if badge.stale {
            Color::DarkGray
        } else {
            priority_color(priority)
        };
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(" {} ", priority.label()),
                Style::default().fg(prio_color).add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Right),
        );
    }
    block
}

#[cfg(test)]
mod tests {
    use crate::models::SessionState;
    use crate::ui::common::buffer_to_string;
    use crate::ui::sessions::fixtures::{fake_session, render, row, NOW};
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    #[test]
    fn card_marks_cold_cache_with_snowflake() {
        let mut s = fake_session();
        let warm: String = (0..7).map(|y| row(&render(&s, 42, 7), y)).collect();
        assert!(!warm.contains('󰜗'), "12m-old session must not be frosted");

        s.last_activity = Some(NOW - 2 * 3_600_000); // 2h — past the cache TTL
        let cold: String = (0..7).map(|y| row(&render(&s, 42, 7), y)).collect();
        // Once in the title (skim surface), once as the footer clock.
        assert_eq!(cold.matches('󰜗').count(), 2, "card:\n{}", cold);
    }

    #[test]
    fn card_task_badge_colors_by_task_and_dims_stale() {
        let live = crate::models::TaskBadge {
            task_id: "tk-live".into(),
            title: "Fix auth".into(),
            priority: Some(crate::tasks::store::TaskPriority::P1),
            stale: false,
        };
        let stale = crate::models::TaskBadge {
            task_id: "tk-gone".into(),
            title: "Old task".into(),
            priority: Some(crate::tasks::store::TaskPriority::P2),
            stale: true,
        };
        let s = fake_session();

        let render_badged = |badge: &crate::models::TaskBadge| {
            let backend = TestBackend::new(42, 7);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| {
                    super::render_card(
                        f,
                        f.area(),
                        &s,
                        Some(badge),
                        super::CardMarks::default(),
                        NOW,
                    )
                })
                .expect("render");
            terminal.backend().buffer().clone()
        };

        // The badge sits on the bottom border in the task's identity color.
        let buf = render_badged(&live);
        let plain = buffer_to_string(&buf);
        assert!(plain.contains("Fix auth"), "badge missing:\n{}", plain);
        let bottom = buf.area().height - 1;
        let fx = (0..buf.area().width)
            .find(|&x| buf[(x, bottom)].symbol() == "F")
            .expect("badge on bottom border");
        assert_eq!(
            buf[(fx, bottom)].style().fg,
            Some(crate::ui::common::task_color("tk-live")),
            "badge not in the task's color"
        );

        // The priority label sits on the bottom-right, in the board's
        // priority hue rather than the task's identity color.
        let px = (0..buf.area().width)
            .rev()
            .find(|&x| buf[(x, bottom)].symbol() == "P")
            .expect("priority label on bottom border");
        assert_eq!(buf[(px + 1, bottom)].symbol(), "1");
        assert_eq!(buf[(px, bottom)].style().fg, Some(Color::LightRed));

        // Distinct tasks may hash to the same hue, but these two don't —
        // and a stale badge always dims to gray instead, priority included.
        assert_ne!(
            crate::ui::common::task_color("tk-live"),
            crate::ui::common::task_color("tk-gone"),
        );
        let buf = render_badged(&stale);
        let ox = (0..buf.area().width)
            .find(|&x| buf[(x, bottom)].symbol() == "O")
            .expect("stale badge on bottom border");
        assert_eq!(buf[(ox, bottom)].style().fg, Some(Color::DarkGray));
        let px = (0..buf.area().width)
            .rev()
            .find(|&x| buf[(x, bottom)].symbol() == "P")
            .expect("stale priority label on bottom border");
        assert_eq!(buf[(px, bottom)].style().fg, Some(Color::DarkGray));

        // Unlinked cards carry no mark.
        let plain = buffer_to_string(&render(&s, 42, 7));
        assert!(
            !plain.contains('\u{f04f9}'),
            "badge on unlinked card:\n{}",
            plain
        );
    }

    #[test]
    fn attention_title_is_a_background_chip() {
        let mut s = fake_session();
        s.state = SessionState::WaitingForInput;
        s.title = Some("Fix auth".into());
        let buf = render(&s, 42, 7);
        let fx = (0..buf.area().width)
            .find(|&x| buf[(x, 0)].symbol() == "F")
            .expect("title text on border row");
        assert_eq!(buf[(fx, 0)].style().bg, Some(Color::Yellow));

        // Processing keeps a plain (no-fill) title — bg is exclusive to
        // "needs you".
        s.state = SessionState::Processing;
        let buf = render(&s, 42, 7);
        let fx = (0..buf.area().width)
            .find(|&x| buf[(x, 0)].symbol() == "F")
            .expect("title text on border row");
        assert_ne!(buf[(fx, 0)].style().bg, Some(Color::Yellow));
    }

    #[test]
    fn processing_card_animates_spinner_and_shows_tool() {
        let mut s = fake_session();
        s.state = SessionState::Processing;
        s.current_tool = Some(crate::conversation::CurrentTool {
            name: "Bash".into(),
            hint: Some("cargo test".into()),
        });
        let buf = render(&s, 42, 7);
        let title = row(&buf, 0);
        assert!(
            title.contains(crate::ui::common::spinner_frame(NOW)),
            "spinner missing from title:\n{}",
            title
        );
        let plain = buffer_to_string(&buf);
        assert!(plain.contains("Bash: cargo test"), "tool line:\n{}", plain);
    }
}
