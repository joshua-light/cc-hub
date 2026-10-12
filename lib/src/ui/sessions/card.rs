use super::card_body::body_lines;
use super::{agent_prefix, badge_color, dot_color, draft_text, CardMarks};
use crate::models::{first_line_truncated, SessionInfo, SessionState, TaskBadge};
use crate::ui::common::{priority_color, state_color, COLD_CACHE_ICON, STATE_DOT};
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
        .title(card_title(session, marks, area.width, now));
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

/// Border weight and colour: thick in the state colour when the session
/// needs you, white under the cursor — the weight stays, so a selected card
/// still says whether it needs you. A handoff mark outranks every colour,
/// and a held session is gray — the weight still says the rest. A working
/// session keeps the quiet border: its blinking dot already says so.
fn border(session: &SessionInfo, marks: CardMarks) -> (BorderType, Color) {
    let CardMarks {
        selected,
        handoff,
        held,
        ..
    } = marks;
    let border_color = if handoff {
        HANDOFF_BLUE
    } else if selected {
        Color::White
    } else if held {
        SEP_GRAY
    } else if session.needs_attention() {
        // The state colour tells a Question (blue) from a plain wait
        // (green).
        state_color(&session.state)
    } else {
        SEP_GRAY
    };

    let border_type = if session.needs_attention() {
        // Weight makes "needs you" categorical, not a hue shift.
        BorderType::Thick
    } else if session.state == SessionState::Inactive {
        BorderType::LightDoubleDashed
    } else {
        BorderType::Rounded
    };
    (border_type, border_color)
}

/// The top-border title, the card's main skim surface: agent badge, state
/// dot, cold-cache mark, then the Haiku title (or `✎ …` while one is
/// being generated, or the name being typed while a rename is open). Only the dot takes the state's colour; the words stay
/// white, bold only on the selected card. The project name is left out:
/// the group header shows it.
fn card_title(session: &SessionInfo, marks: CardMarks, width: u16, now: u64) -> Line<'static> {
    let dot = dot_color(&session.state, marks, now);
    let mut words = Style::default().fg(Color::White);
    if marks.selected {
        words = words.add_modifier(Modifier::BOLD);
    }

    // The snowflake marks a cold prompt cache (restarting beats resuming).
    // Here it takes the title's colour; the ice-blue one is the footer clock.
    let cold_mark = if session.cache_cold(now) {
        format!("{} ", COLD_CACHE_ICON)
    } else {
        String::new()
    };
    // Every branch keeps a space right after the dot, so a bare title (no
    // text yet) still leaves a cell between the dot and the border.
    let prefix = agent_prefix(session);
    let title = match (marks.draft, session.title.as_deref()) {
        (Some(draft), _) => {
            // Both corners, the dot and the space after it are taken.
            let room = (width as usize).saturating_sub(prefix.chars().count() + 4);
            format!(" {}", draft_text(draft, room))
        }
        (None, Some(t)) if !t.is_empty() => format!(" {cold_mark}{t}"),
        _ if session.titling => format!(" {cold_mark}✎ …"),
        _ => format!(" {cold_mark}"),
    };
    Line::from(vec![
        Span::styled(prefix, words),
        Span::styled(STATE_DOT, Style::default().fg(dot)),
        Span::styled(title, words),
    ])
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
        " 󰢨 {} ",
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
    fn only_the_dot_takes_the_state_colour() {
        let mut s = fake_session();
        s.title = Some("Fix auth".into());
        for state in [
            SessionState::WaitingForInput,
            SessionState::Question,
            SessionState::Idle,
            SessionState::Inactive,
        ] {
            s.state = state.clone();
            let buf = render(&s, 42, 7);
            let cell = |sym: &str| {
                (0..buf.area().width)
                    .map(|x| &buf[(x, 0)])
                    .find(|c| c.symbol() == sym)
                    .expect("on the border row")
                    .clone()
            };
            assert_eq!(
                cell(crate::ui::common::STATE_DOT).fg,
                super::state_color(&state)
            );
            let word = cell("F");
            assert_eq!(
                (word.fg, word.bg),
                (Color::White, Color::Reset),
                "{state:?}"
            );
        }
    }

    #[test]
    fn processing_card_blinks_its_dot_and_shows_tool() {
        let mut s = fake_session();
        s.state = SessionState::Processing;
        s.current_tool = Some(crate::conversation::CurrentTool {
            name: "Bash".into(),
            hint: Some("cargo test".into()),
        });
        let buf = render(&s, 42, 7);
        let dot = (0..buf.area().width)
            .map(|x| &buf[(x, 0)])
            .find(|c| c.symbol() == crate::ui::common::STATE_DOT)
            .expect("state dot in title");
        assert_eq!(dot.fg, crate::ui::common::working_blink(NOW));
        let plain = buffer_to_string(&buf);
        assert!(plain.contains("Bash: cargo test"), "tool line:\n{}", plain);
    }
}
