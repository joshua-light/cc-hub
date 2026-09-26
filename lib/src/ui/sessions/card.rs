use super::card_body::body_lines;
use crate::models::{first_line_truncated, SessionInfo, SessionState};
use crate::ui::common::{
    priority_color, spinner_frame, starting_frame, state_color, state_indicator, task_color,
    COLD_CACHE_ICON,
};
use crate::ui::palette::SEP_GRAY;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

pub(super) fn render_card(
    frame: &mut Frame,
    area: Rect,
    session: &SessionInfo,
    badge: Option<&crate::models::TaskBadge>,
    selected: bool,
    now: u64,
) {
    let (indicator, ind_color) = state_indicator(&session.state);
    let indicator = match session.state {
        SessionState::Processing => spinner_frame(now),
        SessionState::Starting => starting_frame(now),
        _ => indicator,
    };

    let border_color = if selected {
        Color::White
    } else if session.needs_attention() || session.state == SessionState::Processing {
        // Question gets its own blue accent so it's visually distinct from
        // a generic WaitingForInput card — same source of truth as the
        // state indicator icon. Processing mirrors that (green frame) so
        // active sessions read as "alive" at a glance, not as ambient.
        state_color(&session.state)
    } else {
        SEP_GRAY
    };

    let border_type = if selected {
        BorderType::Double
    } else if session.needs_attention() {
        // Thick frame + the chip title below make "needs you" a categorical
        // signal, not just a hue shift — Processing shares the colored border
        // but never gets the weight.
        BorderType::Thick
    } else if session.state == SessionState::Inactive {
        BorderType::LightDoubleDashed
    } else {
        BorderType::Rounded
    };

    // Claude is the ~99% default — labelling every card "[Claude]" is pure
    // noise — so the badge is shown only for non-Claude agents.
    let agent_badge = if session.agent_id == "claude" {
        String::new()
    } else {
        format!("[{}] ", session.agent_badge())
    };

    // Border title is the primary skim surface — prepending the Haiku-
    // generated 2-3 word title when available lets users scan what each
    // session is about without having to read the (truncated, often mid-
    // sentence) last user message inside the card body. A `✎` placeholder
    // marks cards with an in-flight Haiku call so the user can tell a
    // pending title from one that's never going to arrive. The project name
    // is intentionally absent: it's already the header of the card's group.
    //
    // Every branch keeps a space immediately after `indicator`: the state
    // glyph is a Nerd Font icon that renders two columns wide but measures as
    // one, so without a trailing cell its second column collides with the
    // border (the bare no-title case `󰂞` is where this bit).
    //
    // A snowflake after the state icon marks a cold prompt cache: the
    // session sat quiet past the cache TTL, so restarting it beats resuming.
    // It rides the title (the primary skim surface) in the title's own
    // color; the ice-blue version lives in the footer clock.
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
    // Attention cards get a solid chip title (black on the state color) —
    // background fill is reserved exclusively for "needs you", so it can't
    // be confused with the colored-but-ambient Processing border at a
    // glance. Everything else keeps colored bold text on the border.
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
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(title, title_style));

    // Task link mark (`L`): the task title on the bottom border, in a color
    // stable per task id — every card of the same task carries the same
    // mark, wherever it sits in the grid. A stale link (task Done or
    // deleted) dims to gray. The task's priority rides the bottom-right
    // corner in the board's priority hue (colored text, not a filled chip —
    // background fill on this card is reserved for "needs you"), and the
    // title's truncation budget shrinks so the two never collide.
    if let Some(badge) = badge {
        let color = if badge.stale {
            Color::DarkGray
        } else {
            task_color(&badge.task_id)
        };
        let prio_w = badge.priority.map_or(0, |p| p.label().chars().count() + 2);
        let label = format!(
            " 󰓹 {} ",
            first_line_truncated(
                &badge.title,
                (area.width as usize).saturating_sub(7 + prio_w).max(4)
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
                .draw(|f| super::render_card(f, f.area(), &s, Some(badge), false, NOW))
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
