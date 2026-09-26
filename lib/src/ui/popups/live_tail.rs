//! The live transcript tail overlay.

use super::transcript::{
    is_placeholder_preview, parse_preview, render_asst_bullet, render_notice_block,
    render_prompt_block, render_thinking, render_tool_bullet, PreviewPart,
};
use crate::app::App;
use crate::ui::common::{centered_rect, format_tokens, wrapped_total_rows};
use crate::ui::palette::DIM_TEXT;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

pub(crate) fn render_live_tail(frame: &mut Frame, area: Rect, app: &mut App) {
    let popup_area = centered_rect(area, 0.85);

    frame.render_widget(Clear, popup_area);

    let lv = match &mut app.live_view {
        Some(lv) => lv,
        None => return,
    };

    let (title, status_color) = if lv.review_mode {
        (" Transcript · review ", Color::Rgb(230, 180, 90))
    } else if lv.auto_scroll {
        (" Live Tail · streaming ", Color::Green)
    } else {
        (" Live Tail · paused ", Color::Yellow)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(80, 120, 150)))
        .title(Span::styled(
            title,
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height < 3 || inner.width < 2 {
        return;
    }

    let content_area = Rect::new(
        inner.x + 1,
        inner.y + 1,
        inner.width.saturating_sub(1),
        inner.height.saturating_sub(2),
    );

    let (lines, highlight_range) = build_live_tail_content(&lv.messages, lv.highlight_msg_idx);
    // The Paragraph wraps, so `.scroll` counts wrapped rows, not logical lines.
    // Any line wider than the popup adds rows the logical count misses, which
    // is what left the bottom rows unreachable and the highlight off-target.
    let wrap_w = content_area.width;
    let total_rows = wrapped_total_rows(&lines, wrap_w);

    lv.total_content_lines = total_rows;

    if lv.auto_scroll && total_rows > content_area.height {
        lv.scroll = total_rows.saturating_sub(content_area.height);
    }

    // One-shot: consuming the flag lets manual scrolls stick afterwards.
    // If the highlight didn't resolve, clear the flag anyway so we don't
    // keep retrying on every frame.
    if lv.scroll_to_highlight.is_some() {
        if let Some((start, _end)) = highlight_range {
            let h = content_area.height.max(1);
            // `start` is a logical-line index; translate it to the wrapped-row
            // offset the scroll actually operates in.
            let target_row = wrapped_total_rows(&lines[..start.min(lines.len())], wrap_w);
            let target = target_row.saturating_sub(h / 3);
            let max_scroll = total_rows.saturating_sub(h);
            lv.scroll = target.min(max_scroll);
            lv.scroll_to_highlight = None;
        } else if !lv.messages.is_empty() {
            lv.scroll_to_highlight = None;
        }
    }

    let max_scroll = total_rows.saturating_sub(content_area.height);
    if lv.scroll > max_scroll {
        lv.scroll = max_scroll;
    }

    let content = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((lv.scroll, 0));
    frame.render_widget(content, content_area);

    let bottom_y = popup_area.y + popup_area.height - 1;

    let hint_text = " ↑/↓ scroll · G bottom · esc close ";
    let hint_width = (hint_text.chars().count() as u16).min(inner.width);
    let hint = Paragraph::new(Line::from(Span::styled(
        hint_text,
        Style::default().fg(DIM_TEXT),
    )));
    let hint_area = Rect::new(inner.x, bottom_y, hint_width, 1);
    frame.render_widget(hint, hint_area);

    // Scroll indicator on the right of the bottom border
    let scroll_info = format!(
        " {}/{} ",
        (lv.scroll as usize).min(total_rows.saturating_sub(1) as usize) + 1,
        total_rows
    );
    let indicator = Paragraph::new(Line::from(Span::styled(
        scroll_info,
        Style::default().fg(DIM_TEXT),
    )))
    .alignment(ratatui::layout::Alignment::Right);

    let indicator_area = Rect::new(inner.x, bottom_y, inner.width, 1);
    frame.render_widget(indicator, indicator_area);
}

fn build_live_tail_content(
    messages: &[crate::models::ConversationMessage],
    highlight_msg_idx: Option<usize>,
) -> (Vec<Line<'static>>, Option<(usize, usize)>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut highlight_range: Option<(usize, usize)> = None;

    if messages.is_empty() {
        lines.push(Line::from(Span::styled(
            "Waiting for messages…",
            Style::default().fg(Color::DarkGray),
        )));
        return (lines, highlight_range);
    }

    let separate = |lines: &mut Vec<Line<'static>>| {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
    };

    // Running session cost. cache_read tokens are the reused prefix — they
    // do count toward the billed input_tokens of the call, but summing them
    // across turns double-counts the same cached prefix over and over, so
    // billed_in tracks only the *new* input contributed by each turn.
    let mut billed_in: u64 = 0;
    let mut total_out: u64 = 0;

    for (idx, msg) in messages.iter().enumerate() {
        if msg.role == "system" {
            continue;
        }

        if msg.role == "user" || msg.role == crate::conversation::AUTOMATION_ROLE {
            let content = msg.content_preview.trim();
            if is_placeholder_preview(content) {
                continue;
            }
            separate(&mut lines);
            if msg.role == "user" {
                render_prompt_block(&mut lines, content);
            } else {
                render_notice_block(&mut lines, content);
            }
            continue;
        }

        if msg.role == "assistant" {
            let is_peak = highlight_msg_idx == Some(idx);
            let start = lines.len();
            if is_peak {
                separate(&mut lines);
                lines.push(Line::from(Span::styled(
                    "  ◆ peak context-growth turn".to_string(),
                    Style::default()
                        .fg(Color::Rgb(20, 20, 20))
                        .bg(Color::Rgb(230, 180, 90))
                        .add_modifier(Modifier::BOLD),
                )));
            }
            for part in parse_preview(&msg.content_preview) {
                separate(&mut lines);
                match part {
                    PreviewPart::Thinking => render_thinking(&mut lines),
                    PreviewPart::Tool(name) => render_tool_bullet(&mut lines, &name),
                    PreviewPart::Text(text) => render_asst_bullet(&mut lines, &text),
                }
            }

            let input = msg.input_tokens.unwrap_or(0);
            let output = msg.output_tokens.unwrap_or(0);
            let cache_read = msg.cache_read_input_tokens.unwrap_or(0);
            let cache_create = msg.cache_creation_input_tokens.unwrap_or(0);
            let ctx = input + cache_read + cache_create;
            let turn_new = input + cache_create;

            billed_in = billed_in.saturating_add(turn_new);
            total_out = total_out.saturating_add(output);

            if ctx > 0 || output > 0 {
                render_turn_stats(&mut lines, turn_new, output, ctx, billed_in, total_out);
            }

            if is_peak && highlight_range.is_none() {
                highlight_range = Some((start, lines.len()));
            }
        }
    }

    (lines, highlight_range)
}

fn render_turn_stats(
    lines: &mut Vec<Line<'static>>,
    turn_in: u64,
    turn_out: u64,
    ctx: u64,
    cum_in: u64,
    cum_out: u64,
) {
    let dim = Style::default().fg(Color::Rgb(95, 95, 115));
    let accent = Style::default().fg(Color::Rgb(170, 150, 205));
    let ctx_accent = Style::default()
        .fg(Color::Rgb(200, 175, 230))
        .add_modifier(Modifier::BOLD);

    let mut spans = vec![
        Span::styled("  └─ ctx ", dim),
        Span::styled(format_tokens(ctx), ctx_accent),
    ];
    spans.push(Span::styled("  · turn +", dim));
    spans.push(Span::styled(format_tokens(turn_in), accent));
    spans.push(Span::styled(" in / ", dim));
    spans.push(Span::styled(format_tokens(turn_out), accent));
    spans.push(Span::styled(" out  · Σ ", dim));
    spans.push(Span::styled(format_tokens(cum_in), accent));
    spans.push(Span::styled(" in / ", dim));
    spans.push(Span::styled(format_tokens(cum_out), accent));
    spans.push(Span::styled(" out", dim));
    lines.push(Line::from(spans));
}
