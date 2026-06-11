//! Sessions tab: the card grid and the per-session detail popup.

use crate::app::App;
use crate::models::{first_line_truncated, short_sid, SessionDetail, SessionInfo, SessionState};
use crate::ui::common::{
    centered_rect, context_window_size, ctx_bar, ctx_color, format_datetime, format_elapsed,
    format_time, format_tokens, format_tool_label, popup_block, short_model, state_color,
    state_indicator,
};
use crate::ui::palette::{CONTEXT_GRAY, MUTED_TEXT, PURPLE, SEP_GRAY};
use crate::ui::{cell_height, now_ms};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

pub(crate) const GROUP_HEADER_HEIGHT: u16 = 1;
pub(crate) const GROUP_GAP: u16 = 1;

pub(crate) fn render_grid(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.sessions.groups.is_empty() {
        let empty = Paragraph::new("No sessions found. Start an agent session to see it here.")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(empty, area);
        return;
    }

    let cols = app.sessions.grid_cols as usize;
    let cell_width = area.width / app.sessions.grid_cols;

    // Compute content-space y offset for each group
    let mut group_offsets: Vec<u16> = Vec::new();
    let mut y_acc: u16 = 0;
    for group in &app.sessions.groups {
        group_offsets.push(y_acc);
        let rows = group.sessions.len().div_ceil(cols) as u16;
        y_acc = y_acc.saturating_add(GROUP_HEADER_HEIGHT + rows * cell_height() + GROUP_GAP);
    }

    // Auto-scroll to keep selected card visible (prefer showing group header too)
    {
        let g_offset = group_offsets[app.sessions.sel_group];
        let card_row = (app.sessions.sel_in_group / cols) as u16;
        let card_y = g_offset + GROUP_HEADER_HEIGHT + card_row * cell_height();
        let card_bottom = card_y + cell_height();

        if card_bottom.saturating_sub(g_offset) <= area.height {
            // Both header and card fit — keep both visible
            if g_offset < app.sessions.grid_scroll {
                app.sessions.grid_scroll = g_offset;
            } else if card_bottom > app.sessions.grid_scroll + area.height {
                app.sessions.grid_scroll = card_bottom.saturating_sub(area.height);
            }
        } else {
            // Just ensure the card itself is visible
            if card_y < app.sessions.grid_scroll {
                app.sessions.grid_scroll = card_y;
            } else if card_bottom > app.sessions.grid_scroll + area.height {
                app.sessions.grid_scroll = card_bottom.saturating_sub(area.height);
            }
        }
    }

    let scroll = app.sessions.grid_scroll;
    let now = now_ms();
    // Build the tmux→role index once per frame; per-card lookup was
    // O(projects × tasks × workers) and dominated re-render cost on hosts
    // with many tasks.
    let roles_by_tmux = app.projects.snapshot.roles_by_tmux();

    for (gi, group) in app.sessions.groups.iter().enumerate() {
        let g_y = group_offsets[gi];

        // Render group header
        let header_sy = g_y as i32 - scroll as i32;
        if header_sy >= 0 && header_sy < area.height as i32 {
            let hy = area.y + header_sy as u16;
            let total = group.sessions.len();
            let attn = group
                .sessions
                .iter()
                .filter(|s| s.needs_attention())
                .count();

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
                Style::default().fg(SEP_GRAY),
            ));

            let header = Paragraph::new(Line::from(spans));
            frame.render_widget(header, Rect::new(area.x, hy, area.width, 1));
        }

        // Render cards for this group
        for (si, session) in group.sessions.iter().enumerate() {
            let col = (si % cols) as u16;
            let row = (si / cols) as u16;

            let card_cy = g_y + GROUP_HEADER_HEIGHT + row * cell_height();
            let card_sy = card_cy as i32 - scroll as i32;

            // Only render if fully visible within the area
            if card_sy < 0 || card_sy + cell_height() as i32 > area.height as i32 {
                continue;
            }

            let x = area.x + col * cell_width;
            let cy = area.y + card_sy as u16;
            let w = if col == app.sessions.grid_cols - 1 {
                area.x + area.width - x
            } else {
                cell_width
            };

            let is_selected = gi == app.sessions.sel_group && si == app.sessions.sel_in_group;
            let cell_area = Rect::new(x, cy, w, cell_height());
            let role = session
                .tmux_session
                .as_deref()
                .and_then(|t| roles_by_tmux.get(t));
            render_card(frame, cell_area, session, role, is_selected, now);
        }
    }
}

/// Braille spinner shown as the title indicator while a session is
/// Processing. The frame index derives from wall-clock time, so it advances
/// on every repaint — at least once a second from the clock tick, faster
/// while scan events stream in. Motion is the point: a turning glyph reads
/// as "alive" where the static gear read as ambient.
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub(crate) fn spinner_frame(now: u64) -> &'static str {
    SPINNER_FRAMES[((now / 120) % SPINNER_FRAMES.len() as u64) as usize]
}

pub(crate) fn render_card(
    frame: &mut Frame,
    area: Rect,
    session: &SessionInfo,
    role: Option<&crate::projects_scan::SessionRole>,
    selected: bool,
    now: u64,
) {
    let (indicator, ind_color) = state_indicator(&session.state);
    let indicator = if session.state == SessionState::Processing {
        spinner_frame(now)
    } else {
        indicator
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

    // Role badge — prepended into the title so a glance tells the user
    // whether a card is an orchestrator or a worker, and which task it's
    // attached to. Workers also get their worktree name (or "RO" for
    // read-only). None for ordinary sessions.
    let role_prefix = match role {
        Some(crate::projects_scan::SessionRole::Orchestrator { task_id, .. }) => Some(format!(
            "★ orch[{}] ",
            crate::orchestrator::short_task_id(task_id)
        )),
        Some(crate::projects_scan::SessionRole::Worker {
            task_id,
            worktree,
            readonly,
            ..
        }) => {
            let suffix = if *readonly {
                "RO".to_string()
            } else {
                worktree.clone().unwrap_or_else(|| "wt".into())
            };
            Some(format!(
                "↳ wkr[{}/{}] ",
                crate::orchestrator::short_task_id(task_id),
                suffix
            ))
        }
        None => None,
    };
    let prefix = role_prefix.as_deref().unwrap_or("");
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
    let title = match session.title.as_deref() {
        Some(t) if !t.is_empty() => format!("{}{}{} {}", prefix, agent_badge, indicator, t),
        _ if session.titling => format!("{}{}{} ✎ …", prefix, agent_badge, indicator),
        _ => format!("{}{}{} ", prefix, agent_badge, indicator),
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(title, title_style));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let inner_w = inner.width as usize;
    let inner_h = inner.height as usize;

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

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
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
        SessionState::Idle | SessionState::Inactive => None,
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
        // gray: state color lives in the chip and border only.
        spans.push(Span::styled(
            format!("󰔟 {}", elapsed),
            Style::default().fg(Color::DarkGray),
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

pub(crate) fn render_popup(frame: &mut Frame, area: Rect, app: &App) {
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
    let title = format!(" {} (PID {}) ", session.project_name, session.pid);

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

pub(crate) fn build_popup_content(detail: &SessionDetail, width: u16) -> Vec<Line<'static>> {
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

#[cfg(test)]
mod tests {
    use crate::agent::AgentKind;
    use crate::models::{SessionInfo, SessionState};
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    /// Fixed render clock; sessions stamp ages relative to this.
    const NOW: u64 = 10_000_000_000;

    fn fake_session() -> SessionInfo {
        SessionInfo {
            agent_id: "claude".into(),
            agent_kind: AgentKind::Claude,
            pid: 4242,
            session_id: "abcd1234efgh".into(),
            cwd: "/tmp/p".into(),
            project_name: "p".into(),
            started_at: NOW - 3_600_000,
            last_activity: Some(NOW - 720_000), // 12m ago
            state: SessionState::Idle,
            last_user_message: None,
            summary: None,
            title: None,
            titling: false,
            model: Some("claude-opus-4-8".into()),
            git_branch: Some("main".into()),
            version: None,
            jsonl_path: None,
            tmux_session: None,
            current_tool: None,
            is_thinking: false,
            context_tokens: None,
            tool_uses_count: 0,
        }
    }

    fn render(s: &SessionInfo, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| super::render_card(f, f.area(), s, None, false, NOW))
            .expect("render");
        terminal.backend().buffer().clone()
    }

    fn row(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buf.area().width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

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
            title.contains(super::spinner_frame(NOW)),
            "spinner missing from title:\n{}",
            title
        );
        let plain = buffer_to_string(&buf);
        assert!(plain.contains("Bash: cargo test"), "tool line:\n{}", plain);
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
