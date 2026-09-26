use crate::app::App;
use crate::models::SessionDetail;
use crate::ui::common::{
    centered_rect, format_datetime, format_time, format_tokens, popup_block, short_model,
    state_color, state_indicator, wrapped_total_rows,
};
use crate::ui::palette::MUTED_TEXT;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::Frame;

pub(crate) fn render_popup(frame: &mut Frame, area: Rect, app: &mut App) {
    let popup_area = centered_rect(area, 0.85);

    frame.render_widget(Clear, popup_area);

    if app.detail_loading {
        frame.render_widget(popup_block(" Loading... "), popup_area);
        return;
    }

    let lines = match &app.detail {
        Some(detail) => {
            let session = &detail.info;
            let title = format!(" {} (PID {}) ", session.project_name, session.pid);
            let block = popup_block(Span::styled(
                title,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
            let inner = block.inner(popup_area);
            frame.render_widget(block, popup_area);
            (build_popup_content(detail, inner.width), inner)
        }
        None => {
            frame.render_widget(popup_block(" No data "), popup_area);
            return;
        }
    };
    let (lines, inner) = lines;

    // The Paragraph wraps, so scroll is in wrapped rows. Clamp the stored
    // scroll to the real bottom each frame; the key handler only
    // saturating_adds, so without this `j` scrolls into blank space while the
    // N/N indicator pins at the end.
    let total_rows = wrapped_total_rows(&lines, inner.width);
    if inner.height > 0 {
        let max_scroll = total_rows.saturating_sub(inner.height);
        if app.render.popup_scroll > max_scroll {
            app.render.popup_scroll = max_scroll;
        }
    }

    let scroll_info = format!(
        " {}/{} ",
        (app.render.popup_scroll as usize).min(total_rows.saturating_sub(1) as usize) + 1,
        total_rows
    );

    let scroll_span = Paragraph::new(Line::from(Span::styled(
        scroll_info,
        Style::default().fg(Color::DarkGray),
    )));
    if inner.height > 0 {
        let indicator_area = Rect::new(
            inner.x,
            popup_area.y + popup_area.height - 1,
            inner.width,
            1,
        );
        frame.render_widget(
            scroll_span.alignment(ratatui::layout::Alignment::Right),
            indicator_area,
        );
    }

    let content = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.render.popup_scroll, 0));

    frame.render_widget(content, inner);
}

fn build_popup_content(detail: &SessionDetail, width: u16) -> Vec<Line<'static>> {
    let session = &detail.info;
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("  ", Style::default().fg(MUTED_TEXT)),
        Span::styled("Path:    ", Style::default().fg(Color::DarkGray)),
        Span::styled(session.cwd.clone(), Style::default().fg(Color::White)),
    ]));

    let mut meta_spans = vec![
        Span::styled("󰚩  ", Style::default().fg(MUTED_TEXT)),
        Span::styled("Agent:  ", Style::default().fg(Color::DarkGray)),
        Span::styled(session.agent_badge(), Style::default().fg(Color::White)),
        Span::styled("   󰘦  ", Style::default().fg(MUTED_TEXT)),
        Span::styled("Branch:  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            session.git_branch.clone().unwrap_or_default(),
            Style::default().fg(Color::Cyan),
        ),
    ];
    if let Some(model) = &session.model {
        meta_spans.push(Span::styled(
            "   󰧑 Model: ",
            Style::default().fg(Color::DarkGray),
        ));
        meta_spans.push(Span::styled(
            short_model(model).to_string(),
            Style::default().fg(Color::White),
        ));
    }
    if let Some(version) = &session.version {
        meta_spans.push(Span::styled("   v", Style::default().fg(Color::DarkGray)));
        meta_spans.push(Span::styled(
            version.clone(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines.push(Line::from(meta_spans));

    let (state_icon, _) = state_indicator(&session.state);
    let sc = state_color(&session.state);
    lines.push(Line::from(vec![
        Span::styled(format!("{} ", state_icon), Style::default().fg(sc)),
        Span::styled("State:   ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{}", session.state), Style::default().fg(sc)),
        Span::styled("   󰥔 Started: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format_datetime(session.started_at),
            Style::default().fg(Color::White),
        ),
    ]));

    lines.push(Line::from(vec![
        Span::styled("󰆏 ", Style::default().fg(MUTED_TEXT)),
        Span::styled("Tokens:  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(
                "{}in / {}out",
                format_tokens(detail.total_input_tokens),
                format_tokens(detail.total_output_tokens)
            ),
            Style::default().fg(Color::White),
        ),
    ]));

    if let Some(summary) = &session.summary {
        lines.push(Line::from(vec![
            Span::styled("󰍡 ", Style::default().fg(MUTED_TEXT)),
            Span::styled("Topic:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(summary.clone(), Style::default().fg(Color::White)),
        ]));
    }

    let sep: String = "─".repeat(width.saturating_sub(1) as usize);
    lines.push(Line::from(Span::styled(
        sep,
        Style::default().fg(Color::Rgb(50, 50, 60)),
    )));
    lines.push(Line::raw(""));

    for msg in &detail.recent_messages {
        let (role_icon, role_label, role_color) = match msg.role.as_str() {
            "user" => ("", "user", Color::Yellow),
            "assistant" => ("󰧑", "asst", Color::Green),
            "system" => ("", "sys ", Color::DarkGray),
            crate::conversation::AUTOMATION_ROLE => ("", "auto", Color::Cyan),
            _ => ("", "??? ", Color::DarkGray),
        };

        let time_str = format_time(msg.timestamp);

        let mut header_spans = vec![
            Span::styled(
                format!("{} {} ", role_icon, role_label),
                Style::default().fg(role_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("󰥔 {}", time_str),
                Style::default().fg(Color::DarkGray),
            ),
        ];

        if let Some(model) = &msg.model {
            let short = short_model(model);
            header_spans.push(Span::styled(
                format!("  󰧑 {}", short),
                Style::default().fg(Color::Rgb(80, 80, 80)),
            ));
        }

        if let Some(stop) = &msg.stop_reason {
            if stop == "tool_use" {
                header_spans.push(Span::styled("   tools", Style::default().fg(Color::Cyan)));
            }
        }

        if let (Some(inp), Some(out)) = (msg.input_tokens, msg.output_tokens) {
            header_spans.push(Span::styled(
                format!("  {}in/{}out", format_tokens(inp), format_tokens(out)),
                Style::default().fg(Color::Rgb(60, 60, 60)),
            ));
        }

        lines.push(Line::from(header_spans));

        for content_line in msg.content_preview.lines() {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    content_line.to_string(),
                    Style::default().fg(Color::Rgb(200, 200, 210)),
                ),
            ]));
        }
        lines.push(Line::raw(""));
    }

    lines
}
