use crate::models::{first_line_truncated, short_sid, SessionInfo, SessionState};
use crate::ui::common::{
    context_window_size, ctx_bar, ctx_color, format_elapsed, format_tool_label, short_model,
    state_indicator, COLD_CACHE_ICON,
};
use crate::ui::palette::{CONTEXT_GRAY, ICE_BLUE, MUTED_TEXT, PURPLE};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// The rows inside a card's border, top to bottom.
pub(super) fn body_lines(
    session: &SessionInfo,
    now: u64,
    inner_w: usize,
    inner_h: usize,
) -> Vec<Line<'static>> {
    // Body order is activity → message → (padding) → identity rows: what
    // the session is doing or needs comes first, identity metadata is
    // pinned to the card floor so it sits in the same place on every card
    // regardless of how many payload lines render above it.
    let mut lines = Vec::new();
    if let Some(activity) = activity_line(session, inner_w) {
        lines.push(activity);
    }

    // Identity rows. With four or more body rows the branch gets a row of
    // its own — real branch names (`refactor/architecture-cleanup`) don't
    // fit beside the model and id — while shorter custom cells fall back to
    // merging all three into one compact row.
    let mut bottom: Vec<Line> = Vec::new();
    if inner_h >= 4 {
        if let Some(branch) = branch_line(session, inner_w) {
            bottom.push(branch);
        }
        bottom.push(model_line(session, inner_w));
    } else {
        bottom.push(meta_line(session, inner_w));
    }
    bottom.push(footer_line(session, now, inner_w));

    // The Haiku title in the border already summarises the session —
    // repeating the (often truncated mid-sentence) last user message below
    // it is noise. Only render the message when no title is available to
    // skim against, and only into rows the activity line left free above
    // the pinned identity rows.
    let msg_budget = inner_h.saturating_sub(bottom.len() + lines.len()).min(2);
    let display_msg = if session.title.as_deref().is_some_and(|t| !t.is_empty()) {
        None
    } else {
        session
            .last_user_message
            .as_ref()
            .or(session.summary.as_ref())
    };
    if let Some(msg) = display_msg {
        lines.extend(message_lines(msg, inner_w, msg_budget));
    }

    let pad = inner_h.saturating_sub(lines.len() + bottom.len());
    for _ in 0..pad {
        lines.push(Line::raw(""));
    }
    lines.extend(bottom);
    lines
}

/// Row 1 of a card body: only what the border chrome *can't* say. The chip
/// title, border weight, and state icon already carry the state itself, so
/// repeating it here as text ("needs input", "idle") would be noise — this
/// row renders the live payload instead: the pending tool a Waiting card
/// wants approved, the question text it wants answered, or the tool a
/// Processing card is running. Dormant states have no payload and get no row.
fn activity_line(session: &SessionInfo, inner_w: usize) -> Option<Line<'static>> {
    // The payload is content, not state — the chip, border and spinner
    // already carry the state color, so the row reads in plain white.
    // Attention payloads keep bold so the thing to approve/answer still
    // pops when scanning a wall of cards.
    match session.state {
        SessionState::Question => {
            // The unresolved tool_use is AskUserQuestion; its hint is the
            // question text — the one thing worth a row.
            let hint = session.current_tool.as_ref()?.hint.clone()?;
            let (icon, _) = state_indicator(&session.state);
            // The icon renders two columns but measures one — count it as 2.
            let hint = first_line_truncated(&hint, inner_w.saturating_sub(3).max(6));
            Some(Line::from(Span::styled(
                format!("{} {}", icon, hint),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )))
        }
        SessionState::WaitingForInput => {
            let tool = session.current_tool.as_ref()?;
            Some(Line::from(Span::styled(
                format_tool_label(tool, inner_w),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )))
        }
        SessionState::Processing => {
            // Tool wins over thinking when both are present — a running tool
            // is always more actionable than recent reasoning. With neither
            // there is no payload: the spinner in the title already says
            // "working", so a filler row would only restate it.
            if let Some(tool) = session.current_tool.as_ref() {
                Some(Line::from(Span::styled(
                    format_tool_label(tool, inner_w),
                    Style::default().fg(Color::White),
                )))
            } else if session.is_thinking {
                Some(Line::from(Span::styled(
                    "󰛨 thinking…".to_string(),
                    Style::default().fg(PURPLE),
                )))
            } else {
                None
            }
        }
        // Starting has no payload by construction: the placeholder's title
        // already says "starting …" and no transcript exists yet.
        SessionState::Starting | SessionState::Idle | SessionState::Inactive => None,
    }
}

/// Dedicated branch row, used when the cell is tall enough: real branch
/// names (`refactor/architecture-cleanup`) need the full card width.
fn branch_line(session: &SessionInfo, inner_w: usize) -> Option<Line<'static>> {
    let branch = session.git_branch.as_deref().filter(|b| !b.is_empty())?;
    Some(Line::from(vec![
        Span::styled("󰘦 ", Style::default().fg(MUTED_TEXT)),
        Span::styled(
            first_line_truncated(branch, inner_w.saturating_sub(3)),
            Style::default().fg(Color::Cyan),
        ),
    ]))
}

/// Model row with the pid:sid debug id hugging the right edge.
fn model_line(session: &SessionInfo, inner_w: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut left_cols = 0usize;
    if let Some(model) = session.model.as_deref().filter(|m| !m.is_empty()) {
        let short = short_model(model);
        spans.push(Span::styled(
            format!("󰧑 {}", short),
            Style::default().fg(Color::DarkGray),
        ));
        left_cols = 3 + short.chars().count();
    }
    let id = format!("{}:{}", session.pid, short_sid(&session.session_id));
    let pad = inner_w.saturating_sub(left_cols + id.chars().count());
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(
        id,
        Style::default().fg(Color::Rgb(50, 50, 60)),
    ));
    Line::from(spans)
}

/// The last user message (or topic summary), dimmed, on up to `budget` rows.
/// Only untitled cards render this — it's the fallback skim surface until
/// the Haiku title arrives.
fn message_lines(msg: &str, inner_w: usize, budget: usize) -> Vec<Line<'static>> {
    let max_w = inner_w.saturating_sub(3); // icon prefix
    if budget == 0 || max_w == 0 {
        return Vec::new();
    }
    let msg = msg.replace('\n', " ");
    let chars: Vec<char> = msg.chars().collect();
    let style = Style::default().fg(CONTEXT_GRAY);
    let icon = Span::styled("󰍡 ", Style::default().fg(MUTED_TEXT));
    if budget == 1 || chars.len() <= max_w {
        return vec![Line::from(vec![
            icon,
            Span::styled(first_line_truncated(&msg, max_w), style),
        ])];
    }
    let first: String = chars[..max_w].iter().collect();
    let rest: String = chars[max_w..].iter().collect();
    vec![
        Line::from(vec![icon, Span::styled(first, style)]),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(first_line_truncated(&rest, max_w), style),
        ]),
    ]
}

/// Compact fallback for short cells: branch, model and pid:sid merged into
/// a single identity row.
fn meta_line(session: &SessionInfo, inner_w: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut left_cols = 0usize;
    if let Some(branch) = session.git_branch.as_deref().filter(|b| !b.is_empty()) {
        spans.push(Span::styled("󰘦 ", Style::default().fg(MUTED_TEXT)));
        spans.push(Span::styled(
            branch.to_string(),
            Style::default().fg(Color::Cyan),
        ));
        left_cols += 3 + branch.chars().count();
    }
    if let Some(model) = session.model.as_deref().filter(|m| !m.is_empty()) {
        let short = short_model(model);
        let sep = if spans.is_empty() { "" } else { " · " };
        spans.push(Span::styled(
            format!("{}󰧑 {}", sep, short),
            Style::default().fg(Color::DarkGray),
        ));
        left_cols += sep.chars().count() + 3 + short.chars().count();
    }
    let id = format!("{}:{}", session.pid, short_sid(&session.session_id));
    let pad = inner_w.saturating_sub(left_cols + id.chars().count());
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(
        id,
        Style::default().fg(Color::Rgb(50, 50, 60)),
    ));
    Line::from(spans)
}

/// Bottom row: liveness numbers — time since last activity, the cumulative
/// tool-call odometer, and the context-window bar. The odometer is the
/// "something is happening" readout: it ticks up within one scan of every
/// tool_use landing in the transcript.
fn footer_line(session: &SessionInfo, now: u64, inner_w: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut left_cols = 0usize;
    if let Some(ts) = session.last_activity {
        let elapsed = format_elapsed(now, ts);
        // On attention cards this clock doubles as the wait age — how long
        // the agent has been blocked on the user — but it stays metadata
        // gray: state color lives in the chip and border only. Past the
        // prompt-cache TTL the clock becomes an ice-blue snowflake: the one
        // exception, because "cold" is exactly what this number measures.
        let (icon, color) = if session.cache_cold(now) {
            (COLD_CACHE_ICON, ICE_BLUE)
        } else {
            ("󰔟", Color::DarkGray)
        };
        spans.push(Span::styled(
            format!("{} {}", icon, elapsed),
            Style::default().fg(color),
        ));
        left_cols += 3 + elapsed.chars().count();
    }
    if session.tool_uses_count > 0 {
        let sep = if spans.is_empty() { "" } else { "  " };
        let count = session.tool_uses_count.to_string();
        // Same metadata gray as the clock and model rows — the odometer is
        // ambient info, not a signal that warrants its own accent.
        spans.push(Span::styled(
            format!("{}󰖷 {}", sep, count),
            Style::default().fg(Color::DarkGray),
        ));
        left_cols += sep.len() + 3 + count.chars().count();
    }
    if let Some(ctx) = session.context_tokens {
        let window = context_window_size(session.model.as_deref().unwrap_or(""));
        let pct = ((ctx as f64 / window as f64) * 100.0).min(999.0);
        let pct_u8 = (pct as u64).min(100) as u8;
        let pct_label = format!(" {:.0}%", pct);
        let bar_w = 8usize;
        let bar_cols = 3 + bar_w + pct_label.chars().count();
        let avail = inner_w.saturating_sub(left_cols);
        if avail > bar_cols {
            spans.push(Span::raw(" ".repeat(avail - bar_cols)));
            spans.push(Span::styled(
                "󰍛 ".to_string(),
                Style::default().fg(ctx_color(pct_u8)),
            ));
            spans.extend(ctx_bar(pct_u8, bar_w));
            spans.push(Span::styled(
                pct_label,
                Style::default().fg(ctx_color(pct_u8)),
            ));
        } else {
            // Card too narrow for the bar — fall back to icon + percent.
            let label = format!("󰍛 {:.0}%", pct);
            let pad = avail.saturating_sub(1 + label.chars().count());
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(label, Style::default().fg(ctx_color(pct_u8))));
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use crate::models::SessionState;
    use crate::ui::common::buffer_to_string;
    use crate::ui::sessions::fixtures::{fake_session, render, row};
    use ratatui::style::Color;

    #[test]
    fn footer_shows_tool_counter_only_when_nonzero() {
        let mut s = fake_session();
        s.tool_uses_count = 142;
        let plain = buffer_to_string(&render(&s, 42, 7));
        assert!(
            plain.contains("\u{f05b7} 142"),
            "counter missing:\n{}",
            plain
        );

        s.tool_uses_count = 0;
        let plain = buffer_to_string(&render(&s, 42, 7));
        assert!(
            !plain.contains('\u{f05b7}'),
            "no uses => no badge:\n{}",
            plain
        );
    }

    #[test]
    fn waiting_card_shows_pending_tool_not_state_label() {
        let mut s = fake_session();
        s.state = SessionState::WaitingForInput;
        // The chip + thick border already say "needs input" — the body must
        // not repeat it as text.
        let plain = buffer_to_string(&render(&s, 42, 7));
        assert!(!plain.contains("needs input"), "label repeated:\n{}", plain);

        s.current_tool = Some(crate::conversation::CurrentTool {
            name: "Bash".into(),
            hint: Some("cargo build".into()),
        });
        let plain = buffer_to_string(&render(&s, 42, 7));
        assert!(
            plain.contains("Bash: cargo build"),
            "pending tool missing:\n{}",
            plain
        );
    }

    #[test]
    fn footer_clock_stays_gray_even_on_attention_cards() {
        // State color lives in the chip and border only — the wait age is
        // readable metadata, not another colored signal.
        let mut s = fake_session();
        for state in [SessionState::WaitingForInput, SessionState::Idle] {
            s.state = state;
            let buf = render(&s, 42, 7);
            let cx = (0..buf.area().width)
                .find(|&x| buf[(x, 5)].symbol() == "1")
                .expect("clock digits on footer row");
            assert_eq!(buf[(cx, 5)].style().fg, Some(Color::DarkGray));
        }
    }

    #[test]
    fn question_card_shows_question_text_not_tool_name() {
        let mut s = fake_session();
        s.state = SessionState::Question;
        s.current_tool = Some(crate::conversation::CurrentTool {
            name: "AskUserQuestion".into(),
            hint: Some("keep the old endpoint?".into()),
        });
        let plain = buffer_to_string(&render(&s, 42, 7));
        assert!(
            plain.contains("keep the old endpoint?"),
            "question text missing:\n{}",
            plain
        );
        assert!(
            !plain.contains("AskUserQuestion"),
            "tool leaked:\n{}",
            plain
        );
    }

    #[test]
    fn message_only_renders_on_untitled_cards() {
        let mut s = fake_session();
        s.last_user_message = Some("please rerun the suite".into());
        let plain = buffer_to_string(&render(&s, 42, 7));
        assert!(
            plain.contains("please rerun the suite"),
            "untitled card needs the message fallback:\n{}",
            plain
        );

        // The Haiku title already summarises the session — the message line
        // is noise next to it.
        s.title = Some("Fix auth".into());
        let plain = buffer_to_string(&render(&s, 42, 7));
        assert!(
            !plain.contains("please rerun the suite"),
            "titled card must not repeat the message:\n{}",
            plain
        );
    }

    #[test]
    fn default_cell_height_gives_branch_its_own_row() {
        // 6-row cell = borders + payload + branch + model + footer. An
        // active card must use every inner row; long branch names get the
        // full card width.
        let mut s = fake_session();
        s.state = SessionState::Processing;
        s.title = Some("Fix auth".into());
        s.git_branch = Some("refactor/architecture-cleanup".into());
        s.current_tool = Some(crate::conversation::CurrentTool {
            name: "Bash".into(),
            hint: Some("cargo test".into()),
        });
        s.tool_uses_count = 12;
        let buf = render(&s, 42, 6);
        assert!(
            row(&buf, 1).contains("Bash: cargo test"),
            "{}",
            row(&buf, 1)
        );
        assert!(
            row(&buf, 2).contains("refactor/architecture-cleanup"),
            "{}",
            row(&buf, 2)
        );
        assert!(
            !row(&buf, 2).contains("opus") && !row(&buf, 2).contains("4242"),
            "branch row must hold only the branch:\n{}",
            row(&buf, 2)
        );
        assert!(row(&buf, 3).contains("opus-4-8"), "{}", row(&buf, 3));
        assert!(row(&buf, 3).contains("4242:abcd1234"), "{}", row(&buf, 3));
        assert!(row(&buf, 4).contains("12m"), "{}", row(&buf, 4));

        // Untitled idle card: the message takes the payload row instead.
        let mut s = fake_session();
        s.last_user_message = Some("ship it".into());
        let buf = render(&s, 42, 6);
        assert!(row(&buf, 1).contains("ship it"), "{}", row(&buf, 1));
    }

    #[test]
    fn compact_height_merges_identity_rows() {
        // At 5 rows and below there is no room for a dedicated branch row —
        // branch, model and id collapse back into one line.
        let mut s = fake_session();
        s.title = Some("Fix auth".into());
        let buf = render(&s, 42, 5);
        let merged = row(&buf, 2);
        assert!(
            merged.contains("main") && merged.contains("opus-4-8"),
            "merged identity row:\n{}",
            merged
        );
        assert!(row(&buf, 3).contains("12m"), "{}", row(&buf, 3));
    }

    #[test]
    fn processing_without_payload_has_no_filler_row() {
        // The spinner + green border already say "working" — a card running
        // nothing concrete must not invent a status row.
        let mut s = fake_session();
        s.state = SessionState::Processing;
        s.title = Some("Fix auth".into());
        let plain = buffer_to_string(&render(&s, 42, 6));
        assert!(!plain.contains("working"), "filler row:\n{}", plain);
    }

    #[test]
    fn identity_rows_pin_to_card_floor() {
        let s = fake_session();
        let buf = render(&s, 42, 7);
        // Inner rows are y=1..=5; identity sits on the last three regardless
        // of how little renders above.
        assert!(
            row(&buf, 3).contains("main"),
            "branch row:\n{}",
            row(&buf, 3)
        );
        assert!(
            row(&buf, 4).contains("4242:abcd1234"),
            "pid:sid:\n{}",
            row(&buf, 4)
        );
        assert!(
            row(&buf, 5).contains("\u{f051f} 12m"),
            "footer clock:\n{}",
            row(&buf, 5)
        );
    }

    #[test]
    fn dormant_cards_skip_the_state_label_and_lead_with_message() {
        let mut s = fake_session();
        s.last_user_message = Some("ship it".into());
        for state in [SessionState::Idle, SessionState::Inactive] {
            s.state = state;
            let buf = render(&s, 42, 7);
            let plain = buffer_to_string(&buf);
            // Border + title icon already say idle/inactive; no payload, no
            // activity row — the message takes the top slot instead.
            assert!(!plain.contains("idle"), "label repeated:\n{}", plain);
            assert!(!plain.contains("inactive"), "label repeated:\n{}", plain);
            assert!(
                row(&buf, 1).contains("ship it"),
                "message not on row 1:\n{}",
                plain
            );
        }
    }

    #[test]
    fn footer_renders_context_bar_with_percent() {
        let mut s = fake_session();
        s.context_tokens = Some(140_000); // 70% of the 200k window
        let buf = render(&s, 42, 7);
        let footer = row(&buf, 5);
        assert!(footer.contains("70%"), "pct missing:\n{}", footer);
        assert!(footer.contains('━'), "bar missing:\n{}", footer);
    }
}
