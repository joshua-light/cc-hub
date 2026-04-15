use crate::app::{App, View};
use crate::models::{SessionDetail, SessionInfo, SessionState};
use chrono::{Local, TimeZone};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

const CELL_HEIGHT: u16 = 8;

pub fn render(frame: &mut Frame, app: &mut App) {
    app.update_grid_cols(frame.area().width);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(frame.area());

    render_title_bar(frame, chunks[0], app);
    render_grid(frame, chunks[1], app);
    render_status_bar(frame, chunks[2], app);

    if app.view == View::Popup {
        render_popup(frame, frame.area(), app);
    }
}

fn render_title_bar(frame: &mut Frame, area: Rect, app: &App) {
    let alive = app.alive_count();
    let total = app.sessions.len();
    let attention = app.sessions.iter().filter(|s| s.needs_attention()).count();

    let mut spans = vec![
        Span::styled(
            " cc-hub ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} alive / {} total", alive, total),
            Style::default().fg(Color::DarkGray),
        ),
    ];

    if attention > 0 {
        spans.push(Span::styled(
            format!("  {} need attention", attention),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans))
            .style(Style::default().bg(Color::Rgb(30, 30, 40)).fg(Color::White)),
        area,
    );
}

fn render_grid(frame: &mut Frame, area: Rect, app: &App) {
    if app.sessions.is_empty() {
        let empty = Paragraph::new("No sessions found. Start a Claude Code session to see it here.")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(empty, area);
        return;
    }

    let cols = app.grid_cols as usize;
    let cell_width = area.width / app.grid_cols;

    for (i, session) in app.sessions.iter().enumerate() {
        let col = (i % cols) as u16;
        let row = (i / cols) as u16;

        let x = area.x + col * cell_width;
        let y = area.y + row * CELL_HEIGHT;

        if y + CELL_HEIGHT > area.y + area.height {
            break;
        }

        let w = if col == app.grid_cols - 1 {
            area.x + area.width - x
        } else {
            cell_width
        };

        let cell_area = Rect::new(x, y, w, CELL_HEIGHT);
        let is_selected = i == app.selected;
        render_card(frame, cell_area, session, is_selected);
    }
}

fn render_card(frame: &mut Frame, area: Rect, session: &SessionInfo, selected: bool) {
    let (indicator, ind_color) = state_indicator(&session.state, session.alive);

    let border_color = if selected {
        Color::White
    } else if session.needs_attention() {
        Color::Yellow
    } else if !session.alive {
        Color::Rgb(50, 50, 50)
    } else {
        Color::Rgb(60, 60, 70)
    };

    let border_type = if selected {
        BorderType::Double
    } else {
        BorderType::Rounded
    };

    let title = format!("{} {}", indicator, session.project_name);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            title,
            Style::default()
                .fg(ind_color)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let mut lines = Vec::new();


    let branch = session.git_branch.as_deref().unwrap_or("");
    lines.push(Line::from(Span::styled(
        branch.to_string(),
        Style::default().fg(Color::Cyan),
    )));

    let model_short = short_model(session.model.as_deref().unwrap_or(""));
    let time_str = format_time(session.last_activity.unwrap_or(session.started_at));

    let inner_w = inner.width as usize;
    let padding = inner_w
        .saturating_sub(model_short.len())
        .saturating_sub(time_str.len());

    lines.push(Line::from(vec![
        Span::styled(model_short.to_string(), Style::default().fg(Color::DarkGray)),
        Span::raw(" ".repeat(padding)),
        Span::styled(time_str, Style::default().fg(Color::DarkGray)),
    ]));

    let sc = state_color(&session.state);
    lines.push(Line::from(Span::styled(
        format!("{}", session.state),
        Style::default().fg(sc),
    )));

    lines.push(Line::raw(""));

    if let Some(msg) = &session.last_user_message {
        let max_w = inner_w.saturating_sub(1);
        let chars: Vec<char> = msg.chars().collect();
        if chars.len() <= max_w {
            lines.push(Line::from(Span::styled(
                msg.clone(),
                Style::default().fg(Color::Rgb(160, 160, 170)),
            )));
        } else {
            let first_line: String = chars[..max_w].iter().collect();
            let remaining: String = chars[max_w..].iter().take(max_w.saturating_sub(3)).collect();
            lines.push(Line::from(Span::styled(
                first_line,
                Style::default().fg(Color::Rgb(160, 160, 170)),
            )));
            let second = if chars.len() > max_w * 2 - 3 {
                format!("{}...", remaining)
            } else {
                remaining
            };
            lines.push(Line::from(Span::styled(
                second,
                Style::default().fg(Color::Rgb(160, 160, 170)),
            )));
        }
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn popup_block<'a>(title: impl Into<ratatui::text::Line<'a>>) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::White))
        .title(title)
}

fn render_popup(frame: &mut Frame, area: Rect, app: &App) {
    let popup_w = (area.width as f32 * 0.85) as u16;
    let popup_h = (area.height as f32 * 0.85) as u16;
    let popup_x = area.x + (area.width.saturating_sub(popup_w)) / 2;
    let popup_y = area.y + (area.height.saturating_sub(popup_h)) / 2;
    let popup_area = Rect::new(popup_x, popup_y, popup_w, popup_h);

    frame.render_widget(Clear, popup_area);

    if app.detail_loading {
        frame.render_widget(popup_block(" Loading... "), popup_area);
        return;
    }

    let detail = match &app.detail {
        Some(d) => d,
        None => {
            frame.render_widget(popup_block(" No data "), popup_area);
            return;
        }
    };

    let session = &detail.info;
    let title = format!(
        " {} (PID {}) ",
        session.project_name, session.pid
    );

    let block = popup_block(Span::styled(
        title,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let lines = build_popup_content(detail, inner.width);

    let total_lines = lines.len() as u16;
    let scroll_info = format!(
        " {}/{} ",
        (app.popup_scroll as usize).min(total_lines.saturating_sub(1) as usize) + 1,
        total_lines
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
        .scroll((app.popup_scroll, 0));

    frame.render_widget(content, inner);
}

fn build_popup_content(detail: &SessionDetail, width: u16) -> Vec<Line<'static>> {
    let session = &detail.info;
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("Path:    ", Style::default().fg(Color::DarkGray)),
        Span::styled(session.cwd.clone(), Style::default().fg(Color::White)),
    ]));

    let mut meta_spans = vec![
        Span::styled("Branch:  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            session.git_branch.clone().unwrap_or_default(),
            Style::default().fg(Color::Cyan),
        ),
    ];
    if let Some(model) = &session.model {
        meta_spans.push(Span::styled("   Model: ", Style::default().fg(Color::DarkGray)));
        meta_spans.push(Span::styled(
            short_model(model).to_string(),
            Style::default().fg(Color::White),
        ));
    }
    if let Some(version) = &session.version {
        meta_spans.push(Span::styled("   v", Style::default().fg(Color::DarkGray)));
        meta_spans.push(Span::styled(version.clone(), Style::default().fg(Color::DarkGray)));
    }
    lines.push(Line::from(meta_spans));

    let sc = state_color(&session.state);
    lines.push(Line::from(vec![
        Span::styled("State:   ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{}", session.state), Style::default().fg(sc)),
        Span::styled("   Started: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format_datetime(session.started_at),
            Style::default().fg(Color::White),
        ),
    ]));

    lines.push(Line::from(vec![
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

    let sep: String = "─".repeat(width.saturating_sub(1) as usize);
    lines.push(Line::from(Span::styled(
        sep,
        Style::default().fg(Color::Rgb(50, 50, 60)),
    )));
    lines.push(Line::raw(""));

    for msg in &detail.recent_messages {
        let (role_label, role_color) = match msg.role.as_str() {
            "user" => ("user", Color::Yellow),
            "assistant" => ("asst", Color::Green),
            "system" => ("sys ", Color::DarkGray),
            _ => ("??? ", Color::DarkGray),
        };

        let time_str = format_time(msg.timestamp);

        let mut header_spans = vec![
            Span::styled(
                format!("[{}] ", role_label),
                Style::default()
                    .fg(role_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(time_str, Style::default().fg(Color::DarkGray)),
        ];

        if let Some(model) = &msg.model {
            let short = short_model(model);
            header_spans.push(Span::styled(
                format!("  {}", short),
                Style::default().fg(Color::Rgb(80, 80, 80)),
            ));
        }

        if let Some(stop) = &msg.stop_reason {
            if stop == "tool_use" {
                header_spans.push(Span::styled(
                    "  [tools]",
                    Style::default().fg(Color::Cyan),
                ));
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

fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let elapsed = app.last_refresh.elapsed().as_secs();
    let refresh_text = if elapsed < 2 {
        "just now".to_string()
    } else {
        format!("{}s ago", elapsed)
    };

    let keybinds = match app.view {
        View::Grid => "h/j/k/l:nav  enter:open  q:quit",
        View::Popup => "j/k:scroll  esc:close  q:close",
    };

    let status = Line::from(vec![
        Span::styled(
            format!(" {} ", keybinds),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("refreshed {} ", refresh_text),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    frame.render_widget(
        Paragraph::new(status).style(Style::default().bg(Color::Rgb(30, 30, 30))),
        area,
    );
}

fn state_indicator(state: &SessionState, alive: bool) -> (&'static str, Color) {
    if !alive {
        return ("[x]", Color::DarkGray);
    }
    match state {
        SessionState::WaitingForInput => ("[!]", Color::Yellow),
        SessionState::Processing => ("[>]", Color::Green),
        SessionState::ToolExecution => ("[T]", Color::Cyan),
        SessionState::Idle => ("[ ]", Color::White),
        SessionState::Dead => ("[x]", Color::DarkGray),
    }
}

fn state_color(state: &SessionState) -> Color {
    state_indicator(state, true).1
}

fn short_model(model: &str) -> &str {
    model.strip_prefix("claude-").unwrap_or(model)
}

fn format_time(timestamp_ms: u64) -> String {
    let secs = (timestamp_ms / 1000) as i64;
    match Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%l:%M %p").to_string(),
        _ => "??:??".to_string(),
    }
}

fn format_datetime(timestamp_ms: u64) -> String {
    let secs = (timestamp_ms / 1000) as i64;
    match Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%b %d %l:%M %p").to_string(),
        _ => "unknown".to_string(),
    }
}

fn format_tokens(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        format!("{}", count)
    }
}
