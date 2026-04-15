use crate::app::{App, View};
use crate::models::{SessionDetail, SessionInfo, SessionState};
use chrono::{Local, TimeZone};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

const CELL_HEIGHT: u16 = 8;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

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

    match app.view {
        View::Popup => render_popup(frame, frame.area(), app),
        View::LiveTail => render_live_tail(frame, frame.area(), app),
        View::Grid => {}
    }
}

fn render_title_bar(frame: &mut Frame, area: Rect, app: &App) {
    let total = app.session_count();
    let attention = app.attention_count();

    let mut spans = vec![
        Span::styled(
            " 󱃾 cc-hub ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} sessions", total),
            Style::default().fg(Color::DarkGray),
        ),
    ];

    if attention > 0 {
        spans.push(Span::styled(
            format!("  󰂞 {} need attention", attention),
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

const GROUP_HEADER_HEIGHT: u16 = 1;
const GROUP_GAP: u16 = 1;

fn render_grid(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.groups.is_empty() {
        let empty =
            Paragraph::new("No sessions found. Start a Claude Code session to see it here.")
                .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(empty, area);
        return;
    }

    let cols = app.grid_cols as usize;
    let cell_width = area.width / app.grid_cols;

    // Compute content-space y offset for each group
    let mut group_offsets: Vec<u16> = Vec::new();
    let mut y_acc: u16 = 0;
    for group in &app.groups {
        group_offsets.push(y_acc);
        let rows = ((group.sessions.len() + cols - 1) / cols) as u16;
        y_acc = y_acc.saturating_add(GROUP_HEADER_HEIGHT + rows * CELL_HEIGHT + GROUP_GAP);
    }

    // Auto-scroll to keep selected card visible (prefer showing group header too)
    {
        let g_offset = group_offsets[app.sel_group];
        let card_row = (app.sel_in_group / cols) as u16;
        let card_y = g_offset + GROUP_HEADER_HEIGHT + card_row * CELL_HEIGHT;
        let card_bottom = card_y + CELL_HEIGHT;

        if card_bottom.saturating_sub(g_offset) <= area.height {
            // Both header and card fit — keep both visible
            if g_offset < app.grid_scroll {
                app.grid_scroll = g_offset;
            } else if card_bottom > app.grid_scroll + area.height {
                app.grid_scroll = card_bottom.saturating_sub(area.height);
            }
        } else {
            // Just ensure the card itself is visible
            if card_y < app.grid_scroll {
                app.grid_scroll = card_y;
            } else if card_bottom > app.grid_scroll + area.height {
                app.grid_scroll = card_bottom.saturating_sub(area.height);
            }
        }
    }

    let scroll = app.grid_scroll;
    let now = now_ms();

    for (gi, group) in app.groups.iter().enumerate() {
        let g_y = group_offsets[gi];

        // Render group header
        let header_sy = g_y as i32 - scroll as i32;
        if header_sy >= 0 && header_sy < area.height as i32 {
            let hy = area.y + header_sy as u16;
            let total = group.sessions.len();
            let attn = group.sessions.iter().filter(|s| s.needs_attention()).count();

            let mut spans = vec![
                Span::styled(
                    format!(" 󰉋 {} ", group.name),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {} sessions", total),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            if attn > 0 {
                spans.push(Span::styled(
                    format!("  󰂞 {}", attn),
                    Style::default().fg(Color::Yellow),
                ));
            }
            // Show cwd path dimmed after the counts
            spans.push(Span::styled(
                format!("  {}", group.cwd),
                Style::default().fg(Color::Rgb(60, 60, 70)),
            ));

            let header = Paragraph::new(Line::from(spans));
            frame.render_widget(header, Rect::new(area.x, hy, area.width, 1));
        }

        // Render cards for this group
        for (si, session) in group.sessions.iter().enumerate() {
            let col = (si % cols) as u16;
            let row = (si / cols) as u16;

            let card_cy = g_y + GROUP_HEADER_HEIGHT + row * CELL_HEIGHT;
            let card_sy = card_cy as i32 - scroll as i32;

            // Only render if fully visible within the area
            if card_sy < 0 || card_sy + CELL_HEIGHT as i32 > area.height as i32 {
                continue;
            }

            let x = area.x + col * cell_width;
            let cy = area.y + card_sy as u16;
            let w = if col == app.grid_cols - 1 {
                area.x + area.width - x
            } else {
                cell_width
            };

            let is_selected = gi == app.sel_group && si == app.sel_in_group;
            let cell_area = Rect::new(x, cy, w, CELL_HEIGHT);
            render_card(frame, cell_area, session, is_selected, now);
        }
    }
}

fn render_card(frame: &mut Frame, area: Rect, session: &SessionInfo, selected: bool, now: u64) {
    let (indicator, ind_color) = state_indicator(&session.state);

    let border_color = if selected {
        Color::White
    } else if session.needs_attention() {
        Color::Yellow
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
    lines.push(Line::from(vec![
        Span::styled(" ", Style::default().fg(Color::Rgb(100, 100, 120))),
        Span::styled(
            branch.to_string(),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            format!("  {}:{}", session.pid, &session.session_id[..8.min(session.session_id.len())]),
            Style::default().fg(Color::Rgb(50, 50, 60)),
        ),
    ]));

    let model_short = short_model(session.model.as_deref().unwrap_or(""));
    let duration_str = format_elapsed(now, session.started_at);

    let model_display = format!("󰧑 {}", model_short);
    let duration_display = format!("󰥔 {}", duration_str);
    let inner_w = inner.width as usize;
    let model_cols = 2 + 1 + model_short.len();
    let duration_cols = 2 + 1 + duration_str.len();
    let padding = inner_w
        .saturating_sub(model_cols)
        .saturating_sub(duration_cols);

    lines.push(Line::from(vec![
        Span::styled(model_display, Style::default().fg(Color::DarkGray)),
        Span::raw(" ".repeat(padding)),
        Span::styled(duration_display, Style::default().fg(Color::DarkGray)),
    ]));

    // Last-activity elapsed time
    let elapsed_str = session.last_activity.map(|ts| format!("󰔟 {}", format_elapsed(now, ts)));

    let mut state_spans: Vec<Span> = Vec::new();
    if let Some(elapsed) = &elapsed_str {
        state_spans.push(Span::styled(elapsed.clone(), Style::default().fg(Color::DarkGray)));
    }

    lines.push(Line::from(state_spans));

    lines.push(Line::raw(""));

    let display_msg = session.last_user_message.as_ref().or(session.summary.as_ref());
    if let Some(msg) = display_msg {
        let max_w = inner_w.saturating_sub(3); // account for icon prefix
        let chars: Vec<char> = msg.chars().collect();
        if chars.len() <= max_w {
            lines.push(Line::from(vec![
                Span::styled("󰍡 ", Style::default().fg(Color::Rgb(100, 100, 120))),
                Span::styled(
                    msg.clone(),
                    Style::default().fg(Color::Rgb(160, 160, 170)),
                ),
            ]));
        } else {
            let first_line: String = chars[..max_w].iter().collect();
            let remaining: String = chars[max_w..].iter().take(max_w.saturating_sub(3)).collect();
            lines.push(Line::from(vec![
                Span::styled("󰍡 ", Style::default().fg(Color::Rgb(100, 100, 120))),
                Span::styled(
                    first_line,
                    Style::default().fg(Color::Rgb(160, 160, 170)),
                ),
            ]));
            let second = if chars.len() > max_w * 2 - 3 {
                format!("{}...", remaining)
            } else {
                remaining
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    second,
                    Style::default().fg(Color::Rgb(160, 160, 170)),
                ),
            ]));
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

fn centered_rect(area: Rect, ratio: f32) -> Rect {
    let w = (area.width as f32 * ratio) as u16;
    let h = (area.height as f32 * ratio) as u16;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

fn render_popup(frame: &mut Frame, area: Rect, app: &App) {
    let popup_area = centered_rect(area, 0.85);

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

fn render_live_tail(frame: &mut Frame, area: Rect, app: &mut App) {
    let popup_area = centered_rect(area, 0.85);

    frame.render_widget(Clear, popup_area);

    let lv = match &mut app.live_view {
        Some(lv) => lv,
        None => return,
    };

    let title = if lv.auto_scroll {
        " Live Tail (streaming) "
    } else {
        " Live Tail (paused) "
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Build message lines
    let lines = build_live_tail_content(&lv.messages);
    let total_lines = lines.len() as u16;

    // Store total for scroll calculations
    lv.total_content_lines = total_lines;

    // Auto-scroll: set scroll so the bottom of content is visible
    if lv.auto_scroll && total_lines > inner.height {
        lv.scroll = total_lines.saturating_sub(inner.height);
    }

    // Clamp scroll
    let max_scroll = total_lines.saturating_sub(inner.height);
    if lv.scroll > max_scroll {
        lv.scroll = max_scroll;
    }

    let content = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((lv.scroll, 0));
    frame.render_widget(content, inner);

    // Scroll indicator on bottom border
    let scroll_info = format!(
        " {}/{} ",
        (lv.scroll as usize).min(total_lines.saturating_sub(1) as usize) + 1,
        total_lines
    );
    let indicator = Paragraph::new(Line::from(Span::styled(
        scroll_info,
        Style::default().fg(Color::DarkGray),
    )))
    .alignment(ratatui::layout::Alignment::Right);

    let indicator_area = Rect::new(
        inner.x,
        popup_area.y + popup_area.height - 1,
        inner.width,
        1,
    );
    frame.render_widget(indicator, indicator_area);
}

fn build_live_tail_content(messages: &[crate::models::ConversationMessage]) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    if messages.is_empty() {
        lines.push(Line::from(Span::styled(
            "Waiting for messages...",
            Style::default().fg(Color::DarkGray),
        )));
        return lines;
    }

    let mut i = 0;
    while i < messages.len() {
        let msg = &messages[i];

        // Skip system messages (turn_duration, stop_hook_summary, etc.)
        if msg.role == "system" {
            i += 1;
            continue;
        }

        // User message — render normally
        if msg.role == "user" {
            render_live_msg(&mut lines, msg, Color::Yellow, "user");
            i += 1;
            continue;
        }

        // Assistant message — check if it's a tool-use turn to collapse
        if msg.role == "assistant" && msg.stop_reason.as_deref() == Some("tool_use") {
            // Collect consecutive tool-use assistant messages into one block
            let mut tools: Vec<String> = Vec::new();
            let mut text_parts: Vec<String> = Vec::new();
            let mut total_out: u64 = 0;
            let first_time = msg.timestamp;

            while i < messages.len()
                && messages[i].role == "assistant"
                && messages[i].stop_reason.as_deref() == Some("tool_use")
            {
                collect_tools_and_text(&messages[i].content_preview, &mut tools, &mut text_parts);
                total_out += messages[i].output_tokens.unwrap_or(0);
                i += 1;
            }

            // Render collapsed tool block
            let time_str = format_time(first_time);
            let tool_list = if tools.is_empty() {
                "tools".to_string()
            } else {
                tools.join(", ")
            };

            lines.push(Line::from(vec![
                Span::styled(
                    "[tools] ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(time_str, Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("  {}", tool_list),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    format!("  {}out", format_tokens(total_out)),
                    Style::default().fg(Color::Rgb(60, 60, 60)),
                ),
            ]));

            // Show any text the assistant said during tool use
            for text in &text_parts {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        text.clone(),
                        Style::default().fg(Color::Rgb(180, 180, 190)),
                    ),
                ]));
            }

            lines.push(Line::raw(""));
            continue;
        }

        // Assistant end_turn — this is the real response text
        if msg.role == "assistant" {
            render_live_msg(&mut lines, msg, Color::Green, "asst");
            i += 1;
            continue;
        }

        i += 1;
    }

    lines
}

/// Render a single message (user or assistant end_turn).
fn render_live_msg(
    lines: &mut Vec<Line<'static>>,
    msg: &crate::models::ConversationMessage,
    color: Color,
    label: &str,
) {
    let time_str = format_time(msg.timestamp);

    let mut header = vec![
        Span::styled(
            format!("[{}] ", label),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(time_str, Style::default().fg(Color::DarkGray)),
    ];

    if let Some(model) = &msg.model {
        header.push(Span::styled(
            format!("  {}", short_model(model)),
            Style::default().fg(Color::Rgb(80, 80, 80)),
        ));
    }

    if let (Some(inp), Some(out)) = (msg.input_tokens, msg.output_tokens) {
        header.push(Span::styled(
            format!("  {}in/{}out", format_tokens(inp), format_tokens(out)),
            Style::default().fg(Color::Rgb(60, 60, 60)),
        ));
    }

    lines.push(Line::from(header));

    // Show content, skip if it's just tool references
    let content = &msg.content_preview;
    if !content.is_empty() && content != "(no content)" && content != "(no text content)" {
        for content_line in content.lines() {
            let trimmed = content_line.trim();
            if trimmed.is_empty() {
                continue;
            }
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    trimmed.to_string(),
                    Style::default().fg(Color::Rgb(200, 200, 210)),
                ),
            ]));
        }
    }

    lines.push(Line::raw(""));
}

/// Extract tool names and text snippets from a content_preview string.
fn collect_tools_and_text(preview: &str, tools: &mut Vec<String>, texts: &mut Vec<String>) {
    for part in preview.split("[tool: ") {
        if tools.is_empty() && !part.contains(']') {
            // Text before first tool reference
            let trimmed = part.trim();
            if !trimmed.is_empty()
                && trimmed != "(no text content)"
                && trimmed != "(no content)"
                && trimmed != "[thinking...]"
            {
                texts.push(trimmed.to_string());
            }
        } else if let Some(end) = part.find(']') {
            let name = &part[..end];
            if !tools.contains(&name.to_string()) {
                tools.push(name.to_string());
            }
            // Text after the tool reference
            let after = part[end + 1..].trim();
            if !after.is_empty()
                && after != "(no text content)"
                && after != "(no content)"
            {
                texts.push(after.to_string());
            }
        }
    }
}

fn build_popup_content(detail: &SessionDetail, width: u16) -> Vec<Line<'static>> {
    let session = &detail.info;
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("  ", Style::default().fg(Color::Rgb(100, 100, 120))),
        Span::styled("Path:    ", Style::default().fg(Color::DarkGray)),
        Span::styled(session.cwd.clone(), Style::default().fg(Color::White)),
    ]));

    let mut meta_spans = vec![
        Span::styled("  ", Style::default().fg(Color::Rgb(100, 100, 120))),
        Span::styled("Branch:  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            session.git_branch.clone().unwrap_or_default(),
            Style::default().fg(Color::Cyan),
        ),
    ];
    if let Some(model) = &session.model {
        meta_spans.push(Span::styled("   󰧑 Model: ", Style::default().fg(Color::DarkGray)));
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
        Span::styled("󰆏 ", Style::default().fg(Color::Rgb(100, 100, 120))),
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
            Span::styled("󰍡 ", Style::default().fg(Color::Rgb(100, 100, 120))),
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
            _ => ("", "??? ", Color::DarkGray),
        };

        let time_str = format_time(msg.timestamp);

        let mut header_spans = vec![
            Span::styled(
                format!("{} {} ", role_icon, role_label),
                Style::default()
                    .fg(role_color)
                    .add_modifier(Modifier::BOLD),
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
                header_spans.push(Span::styled(
                    "   tools",
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
        View::Grid => "h/j/k/l:nav  enter:attach  i:info  f:focus  q:quit",
        View::Popup => "j/k:scroll  esc:close  q:close",
        View::LiveTail => "j/k:scroll  G:bottom  esc:close",
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

fn state_indicator(state: &SessionState) -> (&'static str, Color) {
    match state {
        SessionState::Processing => ("󰑮", Color::Green),
        SessionState::WaitingForInput => ("󰂞", Color::Yellow),
        SessionState::Idle => ("󰒲", Color::Rgb(100, 100, 120)),
    }
}

fn state_color(state: &SessionState) -> Color {
    state_indicator(state).1
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

fn format_elapsed(now: u64, from_ms: u64) -> String {
    let secs = now.saturating_sub(from_ms) / 1000;
    format_duration_secs(secs)
}

fn format_duration_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("{}h", h)
        } else {
            format!("{}h {}m", h, m)
        }
    } else {
        let d = secs / 86400;
        let h = (secs % 86400) / 3600;
        if h == 0 {
            format!("{}d", d)
        } else {
            format!("{}d {}h", d, h)
        }
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
