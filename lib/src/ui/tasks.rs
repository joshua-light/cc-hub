//! Tasks-tab body: a kanban board (To-Do · In Progress · Done by default)
//! over the personal task store. Visually a sibling of the Projects kanban,
//! but each card is a flat task, optionally annotated with its bound agent
//! session's live state (resolved by tmux name, same as project cards).
//! Planning holds cards whose agent is drafting a plan; Space approves it and
//! the card moves to In Progress. The Planning column is opt-in
//! (`ui.show_planning_column = true`); when hidden its cards fold into In
//! Progress.

use crate::app::{visible_task_columns, App, View};
use crate::models::{self, SessionInfo, SessionState};
use crate::orchestrator::{TaskState, TaskStatus};
use crate::ui::common::{centered_rect, popup_block, priority_color};
use crate::ui::now_ms;
use crate::ui::palette::{
    ACCENT_BLUE, DIM_TEXT, DOT_IDLE, FAINT_TEXT, LABEL_GRAY, META_GRAY, TAG_SLATE,
};
use crate::ui::projects::{
    classify_artifact, evidence_card_header, read_text_excerpt, truncated_footer, CardKind,
};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use std::collections::HashMap;
use std::path::Path;

pub fn render_tasks_body(frame: &mut Frame, area: Rect, app: &App) {
    if area.height < 3 || area.width < 30 {
        let hint = Paragraph::new("(terminal too narrow — resize or switch to Sessions)")
            .alignment(Alignment::Center)
            .style(Style::default().fg(DOT_IDLE))
            .wrap(Wrap { trim: false });
        frame.render_widget(hint, area);
        return;
    }
    // Filter bar: one row above the columns, shown while editing (`/`,
    // live cursor) and as long as a committed filter narrows the board —
    // an invisible filter would read as vanished tasks.
    let mut area = area;
    let editing = app.view == View::TaskFilter;
    if editing || !app.tasks.filter.is_empty() {
        let bar = Rect { height: 1, ..area };
        area = Rect {
            y: area.y + 1,
            height: area.height - 1,
            ..area
        };
        render_filter_bar(frame, bar, app, editing);
    }
    // Column set is config-driven: Planning is optional, so split the row
    // into equal shares of however many columns are visible.
    let columns = visible_task_columns();
    let n = columns.len() as u32;
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            columns
                .iter()
                .map(|_| Constraint::Ratio(1, n))
                .collect::<Vec<_>>(),
        )
        .split(area);

    let sessions_by_tmux = app.sessions_by_tmux();
    let now_secs = now_ms() / 1000;
    for (col_idx, status) in columns.iter().enumerate() {
        render_task_column(
            frame,
            cols[col_idx],
            app,
            col_idx,
            *status,
            &sessions_by_tmux,
            now_secs,
        );
    }
}

/// The one-row filter strip: query (with a cursor while editing), how many
/// cards survive across the visible columns, and the key hints for the
/// current mode.
fn render_filter_bar(frame: &mut Frame, area: Rect, app: &App, editing: bool) {
    let matches: usize = (0..visible_task_columns().len())
        .map(|c| app.tasks.column_len(c))
        .sum();
    let mut query = app.tasks.filter.clone();
    if editing {
        query.push('▎');
    }
    let hint = if editing {
        "  enter:apply  esc:clear"
    } else {
        "  /:edit  esc:clear"
    };
    let line = Line::from(vec![
        Span::styled(" / ", Style::default().fg(ACCENT_BLUE)),
        Span::styled(
            query,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "  ({} match{})",
                matches,
                if matches == 1 { "" } else { "es" }
            ),
            Style::default().fg(LABEL_GRAY),
        ),
        Span::styled(hint, Style::default().fg(DIM_TEXT)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// Body-line budget for one attachment's inline excerpt in the Task Info
/// popup. Fixed and modest — the popup is a reference list, not a reader;
/// `o` opens the full document externally.
const INFO_EXCERPT_LINES: usize = 6;

/// Task Info popup for the focused board card: prompt + metadata up top, then
/// every attachment as a card — text files inline a short excerpt, URLs and
/// media render a one-line pointer. `j`/`k` select, `c` copies the stored
/// path, `o` opens externally, `x` removes, `a` attaches another.
pub(crate) fn render_task_info(frame: &mut Frame, area: Rect, app: &mut App) {
    let popup_area = centered_rect(area, 0.8);
    frame.render_widget(Clear, popup_area);

    let Some(t) = app.selected_board_task().cloned() else {
        frame.render_widget(popup_block(" Task — nothing focused "), popup_area);
        return;
    };

    let title = match t.title.as_deref().filter(|s| !s.is_empty()) {
        Some(name) => format!(
            " Task · {} · {} ",
            crate::orchestrator::short_task_id(&t.task_id),
            name,
        ),
        None => format!(
            " Task · {} ",
            crate::orchestrator::short_task_id(&t.task_id)
        ),
    };
    let block = popup_block(Span::styled(
        title,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);
    if inner.height < 2 || inner.width == 0 {
        return;
    }

    let now_secs = now_ms() / 1000;
    let n_attach = t.artifacts.len();
    let sel = if n_attach == 0 {
        0
    } else {
        app.tasks.info_sel.min(n_attach - 1)
    };

    // ── Canvas: header + prompt + attachment cards, one Line per row ──────
    let (status_icon, status_accent) = crate::ui::common::task_status_meta(t.status);
    let mut header_spans = vec![
        Span::styled(
            format!("{} {}", status_icon, t.status.board_label()),
            Style::default()
                .fg(status_accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            t.priority.label().to_string(),
            Style::default().fg(priority_color(t.priority)),
        ),
        Span::raw("  "),
        Span::styled(
            models::relative_age(now_secs.saturating_sub(t.created_at.max(0) as u64)),
            Style::default().fg(META_GRAY),
        ),
        Span::raw("  "),
        Span::styled(
            format!(
                "📎 {} attachment{}",
                n_attach,
                if n_attach == 1 { "" } else { "s" }
            ),
            Style::default().fg(Color::Rgb(150, 130, 200)),
        ),
    ];
    if !t.tags.is_empty() {
        let tags = t
            .tags
            .iter()
            .map(|tag| format!("#{}", tag))
            .collect::<Vec<_>>()
            .join(" ");
        header_spans.push(Span::raw("  "));
        header_spans.push(Span::styled(tags, Style::default().fg(TAG_SLATE)));
    }
    let mut lines: Vec<Line<'static>> = vec![Line::from(header_spans), Line::raw("")];

    let stats_text = match &t.stats {
        Some(stats) => format!(
            "Task Stats · {} tokens · {} · {} sessions\nInput {} · Output {} · Cache read {} · Cache write {}",
            stats.total_tokens(), stats.cost_label(), stats.sessions, stats.input_tokens, stats.output_tokens,
            stats.cache_read_tokens, stats.cache_creation_tokens,
        ),
        None => "Task Stats · usage unavailable (awaiting session transcripts)".into(),
    };
    for line in stats_text.lines() {
        for segment in wrap_text(line, inner.width as usize) {
            lines.push(Line::from(Span::styled(
                segment,
                Style::default().fg(META_GRAY),
            )));
        }
    }
    lines.push(Line::raw(""));
    // Keep room for attachments below the task prompt.
    let prompt_lines = wrap_text(&t.prompt, inner.width as usize);
    let capped = prompt_lines.len() > 8;
    for seg in prompt_lines.into_iter().take(8) {
        lines.push(Line::from(Span::styled(
            seg,
            Style::default().fg(Color::Rgb(200, 200, 210)),
        )));
    }
    if capped {
        lines.push(Line::from(Span::styled("…", Style::default().fg(DIM_TEXT))));
    }
    lines.push(Line::raw(""));

    // ── Attachment cards ──────────────────────────────────────────────────
    // Track each card's (top, end) canvas rows so the scroll clamp below can
    // keep the selected card in view.
    let mut card_spans: Vec<(u16, u16)> = Vec::with_capacity(n_attach);
    if n_attach == 0 {
        lines.push(Line::from(Span::styled(
            "  (no attachments — a attaches a file or URL, p pastes the clipboard as a note)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    for (i, a) in t.artifacts.iter().enumerate() {
        let top = lines.len() as u16;
        let is_lead = t.lead_artifact == Some(i);
        lines.push(evidence_card_header(a, i == sel, is_lead));
        match classify_artifact(a) {
            CardKind::Text | CardKind::Diff => {
                let path = Path::new(&a.path);
                match read_text_excerpt(path, 8 * 1024) {
                    None => {
                        let msg = if std::fs::metadata(path).is_ok() {
                            Span::styled(
                                "  (binary file — open externally with `o`)",
                                Style::default().fg(Color::DarkGray),
                            )
                        } else {
                            Span::styled(
                                format!("  (cannot read {})", a.path),
                                Style::default().fg(Color::Rgb(220, 100, 100)),
                            )
                        };
                        lines.push(Line::from(msg));
                    }
                    Some((content, truncated)) => {
                        let shown = content.len().min(INFO_EXCERPT_LINES);
                        for s in content.iter().take(shown) {
                            lines.push(Line::from(Span::styled(
                                format!("  {}", s),
                                Style::default().fg(Color::Gray),
                            )));
                        }
                        let hidden = content.len().saturating_sub(shown) + truncated;
                        if hidden > 0 {
                            lines.push(truncated_footer(hidden));
                        }
                    }
                }
            }
            CardKind::Url => {
                lines.push(Line::from(Span::styled(
                    format!("  {}", a.path),
                    Style::default().fg(ACCENT_BLUE),
                )));
            }
            CardKind::Image => {
                lines.push(Line::from(Span::styled(
                    "  (image — press `o` to open)",
                    Style::default().fg(Color::DarkGray),
                )));
            }
            CardKind::Video => {
                lines.push(Line::from(Span::styled(
                    "  (video — press `o` to open)",
                    Style::default().fg(Color::DarkGray),
                )));
            }
            CardKind::Fallback => {
                lines.push(Line::from(Span::styled(
                    format!("  {}", a.path),
                    Style::default().fg(FAINT_TEXT),
                )));
            }
        }
        lines.push(Line::raw(""));
        card_spans.push((top, lines.len() as u16));
    }

    // ── Scroll clamp + draw ───────────────────────────────────────────────
    let body_h = inner.height - 1;
    let total = lines.len() as u16;
    let mut scroll = app.render.task_info_scroll;
    if let Some(&(top, end)) = card_spans.get(sel) {
        // Keep the selected card visible, same contract as the Result popup.
        let h = end.saturating_sub(top);
        if top < scroll {
            scroll = top;
        } else if top + h > scroll + body_h {
            scroll = (top + h).saturating_sub(body_h);
        }
    }
    scroll = scroll.min(total.saturating_sub(body_h));
    app.render.task_info_scroll = scroll;

    let body_area = Rect::new(inner.x, inner.y, inner.width, body_h);
    // No wrap: the scroll math above assumes one visual row per canvas line;
    // over-long excerpt/URL lines clip instead of pushing content down.
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), body_area);

    let footer_area = Rect::new(inner.x, inner.y + body_h, inner.width, 1);
    let pos = if n_attach == 0 {
        "attachment —".to_string()
    } else {
        format!("attachment {}/{}", sel + 1, n_attach)
    };
    let hint = format!(
        " {}   j/k:select   a:attach   p:paste note   c:copy path   o:open   x:remove   esc/v:close ",
        pos
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        ))),
        footer_area,
    );
}

fn column_meta(status: TaskStatus) -> (&'static str, &'static str, Color) {
    match status {
        // Orchestrated-only states never render as board columns.
        TaskStatus::Review | TaskStatus::Merging => ("", "", Color::DarkGray),
        // Icon/accent come from the shared status palette so the columns,
        // the Projects kanban, and the task-link picker read the same.
        _ => {
            let (icon, accent) = crate::ui::common::task_status_meta(status);
            (status.board_label(), icon, accent)
        }
    }
}

fn render_task_column(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    col_idx: usize,
    status: TaskStatus,
    sessions_by_tmux: &HashMap<&str, &SessionInfo>,
    now_secs: u64,
) {
    let (label, icon, accent) = column_meta(status);
    // Display order, not board order: live columns put needs-input cards
    // first, frozen at tab entry so scan ticks can't reorder cards under
    // the cursor (see `App::task_display_column`); the cursor indexes the
    // same list. With Planning hidden, In Progress also carries its cards.
    let tasks = app.task_display_column(status);
    let count = tasks.len();
    let col_focused = app.tasks.col == col_idx;
    // Done cards use their second content row for usage stats.
    let card_height: u16 = 5;
    let gap: u16 = 0;
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let max_cards =
        ((inner.height as u32 + gap as u32) / (card_height as u32 + gap as u32)) as usize;
    let sel = if count == 0 {
        0
    } else if col_focused {
        app.tasks.row.min(count - 1)
    } else {
        0
    };
    let scroll_top = if count == 0 || max_cards == 0 || sel < max_cards {
        0
    } else {
        sel + 1 - max_cards
    };
    let hidden_below = count.saturating_sub(scroll_top.saturating_add(max_cards));

    let (border_type, border_style, title_style) = if col_focused {
        (
            BorderType::Double,
            Style::default().fg(accent),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        )
    } else {
        (
            BorderType::Rounded,
            Style::default().fg(Color::Rgb(60, 60, 80)),
            Style::default().fg(LABEL_GRAY),
        )
    };
    let mut title_spans = vec![
        Span::raw(" "),
        Span::styled(format!("{} ", icon), Style::default().fg(accent)),
        Span::styled(label.to_string(), title_style),
        Span::styled(
            format!(" ({}) ", count),
            Style::default().fg(Color::Rgb(140, 140, 165)),
        ),
    ];
    if scroll_top > 0 || hidden_below > 0 {
        let last_visible = scroll_top.saturating_add(max_cards).min(count);
        title_spans.push(Span::styled(
            format!(" · {}-{} ", scroll_top + 1, last_visible),
            Style::default().fg(DIM_TEXT),
        ));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style)
        .title(Line::from(title_spans));
    frame.render_widget(block, area);

    if tasks.is_empty() {
        let empty_hint = match status {
            TaskStatus::Backlog => "No tasks — press a to add one",
            TaskStatus::Planning => "Nothing planning — s hands a task to an agent",
            TaskStatus::Running => "Nothing running — Space approves a plan",
            TaskStatus::Done => "Nothing done yet",
            // Orchestrated-only states never render as board columns.
            TaskStatus::Review | TaskStatus::Merging => "",
        };
        let hint = Paragraph::new(Line::from(Span::styled(
            empty_hint,
            Style::default().fg(Color::Rgb(70, 70, 90)),
        )))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: false });
        frame.render_widget(hint, inner);
        return;
    }

    let mut y = inner.y;
    for (rel, t) in tasks.iter().enumerate().skip(scroll_top).take(max_cards) {
        let card_area = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: card_height,
        };
        let selected = col_focused && rel == sel;
        render_task_card(
            frame,
            card_area,
            t,
            selected,
            status,
            accent,
            sessions_by_tmux,
            now_secs,
        );
        y = y.saturating_add(card_height + gap);
        if y >= inner.y + inner.height {
            break;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_task_card(
    frame: &mut Frame,
    area: Rect,
    t: &TaskState,
    selected: bool,
    status: TaskStatus,
    accent: Color,
    sessions_by_tmux: &HashMap<&str, &SessionInfo>,
    now_secs: u64,
) {
    let (border_type, border_style) = if selected {
        (BorderType::Double, Style::default().fg(accent))
    } else {
        (
            BorderType::Rounded,
            Style::default().fg(Color::Rgb(60, 60, 80)),
        )
    };
    // Priority badge rides the top-right of the border (`P1`–`P4`): black
    // text on a color-filled chip, prominent without stealing a text row. It
    // keeps its own colours even when the selected border turns the accent.
    let priority_badge = Line::from(Span::styled(
        format!(" {} ", t.priority.label()),
        Style::default()
            .fg(Color::Black)
            .bg(priority_color(t.priority))
            .add_modifier(Modifier::BOLD),
    ))
    .alignment(Alignment::Right);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style)
        .title(priority_badge);
    // Tags ride the top-left of the border, mirroring the priority chip on the
    // right. Budget the width against what the priority chip leaves (corners +
    // chip + a one-column gap) so the two badges never collide; overflow folds
    // into a `+N` marker.
    let prio_w = t.priority.label().chars().count() + 2;
    let tag_budget = (area.width as usize).saturating_sub(2 + prio_w + 1);
    if let Some(text) = tags_title_text(&t.tags, tag_budget) {
        block = block.title(
            Line::from(Span::styled(text, Style::default().fg(TAG_SLATE)))
                .alignment(Alignment::Left),
        );
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let done = status == TaskStatus::Done;
    let text_style = if done {
        Style::default().fg(DIM_TEXT).add_modifier(Modifier::ITALIC)
    } else if selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Rgb(200, 200, 210))
    };

    let stats_rows = usize::from(done);
    let text_rows = (inner.height as usize)
        .saturating_sub(1 + stats_rows)
        .max(1);
    let mut lines: Vec<Line> = wrap_text(&t.prompt, inner.width as usize)
        .into_iter()
        .take(text_rows)
        .map(|seg| Line::from(Span::styled(seg, text_style)))
        .collect();

    // Pin the meta row to the card's bottom edge so short and wrapped texts
    // produce the same silhouette.
    while lines.len() < text_rows {
        lines.push(Line::raw(""));
    }
    if done {
        let label = t
            .stats
            .as_ref()
            .map(|stats| stats.compact())
            .unwrap_or_else(|| "Stats unavailable".into());
        lines.push(Line::from(Span::styled(
            label,
            Style::default().fg(META_GRAY),
        )));
    }
    lines.push(meta_line(t, status, sessions_by_tmux, now_secs));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// One row of facts the column/border can't already say: how old the task
/// is, and — once an agent is bound — who runs it, where, and what state the
/// session is in right now.
fn meta_line(
    t: &TaskState,
    status: TaskStatus,
    sessions_by_tmux: &HashMap<&str, &SessionInfo>,
    now_secs: u64,
) -> Line<'static> {
    let age_style = Style::default().fg(META_GRAY);
    // Attachment chip: rendered at the end of either branch so a card with
    // reference docs is recognizable from the board.
    let clip = (!t.artifacts.is_empty())
        .then(|| Span::styled(format!("  📎{}", t.artifacts.len()), age_style));
    if status == TaskStatus::Done {
        let when = t.done_at.unwrap_or(t.created_at);
        let mut spans = vec![Span::styled(
            format!(
                "✓ {}",
                models::relative_age(now_secs.saturating_sub(when.max(0) as u64))
            ),
            age_style,
        )];
        // A done task that ran through an agent keeps its transcript — `f`
        // still opens/resumes it — so say who ran it and where. Marking done
        // closes the live session, but one can outlive that (close failed,
        // board edited by hand), so a survivor still gets the brighter dot.
        if t.tmux.is_some() || t.session_id.is_some() {
            let agent = t.agent_id.as_deref().unwrap_or("agent");
            spans.push(Span::styled(format!("  󰚩 {}", agent), age_style));
            if let Some(dir) = t.cwd.as_deref().map(dir_basename) {
                spans.push(Span::styled(format!(" · {}", dir), age_style));
            }
            if t.tmux
                .as_deref()
                .is_some_and(|n| sessions_by_tmux.contains_key(n))
            {
                spans.push(Span::styled(
                    "  ● live",
                    Style::default().fg(Color::LightGreen),
                ));
            }
        }
        spans.extend(clip);
        return Line::from(spans);
    }

    let mut spans: Vec<Span> = Vec::new();
    if let Some(tmux) = t.tmux.as_deref() {
        let (glyph, label, color) = match sessions_by_tmux.get(tmux) {
            Some(s) => match s.state {
                SessionState::Processing => ("⟳", "working", Color::LightYellow),
                SessionState::WaitingForInput | SessionState::Question => {
                    ("󰂞", "needs input", Color::Yellow)
                }
                // An idle planning agent has finished its turn — the plan is
                // sitting in the transcript waiting for a verdict. Keyed off
                // the card's own status so it still reads "plan ready" when a
                // Planning card is folded into a hidden-Planning In Progress
                // column.
                SessionState::Idle if t.status == TaskStatus::Planning => {
                    ("●", "plan ready", Color::LightGreen)
                }
                // An idle implementation agent may be stalled; explicit
                // activity records below identify completed work.
                SessionState::Idle if t.status == TaskStatus::Running => {
                    ("●", "idle — check progress", Color::LightCyan)
                }
                SessionState::Idle => ("●", "idle", Color::LightGreen),
                // App-synthesized spawn placeholder; task-linked sessions come
                // from the scanner so this arm is exhaustiveness-only.
                SessionState::Starting => ("◌", "starting…", DOT_IDLE),
                SessionState::Inactive => ("○", "inactive", DOT_IDLE),
            },
            // Spawned but not scanned yet, or the tmux died. With a resolved
            // session id `f` resumes; before resolution it can only hint.
            None if t.session_id.is_some() => ("○", "gone — f resumes", DOT_IDLE),
            None => ("○", "starting…", DOT_IDLE),
        };
        spans.push(Span::styled(
            format!(
                "{} {}",
                glyph,
                crate::task_activity::label(&t.task_id)
                    .as_deref()
                    .unwrap_or(label)
            ),
            Style::default().fg(color),
        ));
        if let Some(dir) = t.cwd.as_deref().map(dir_basename) {
            spans.push(Span::styled("  ", age_style));
            spans.push(Span::styled(dir, Style::default().fg(ACCENT_BLUE)));
        }
        spans.push(Span::styled("  ", age_style));
    }
    spans.push(Span::styled(
        models::relative_age(now_secs.saturating_sub(t.created_at.max(0) as u64)),
        age_style,
    ));
    spans.extend(clip);
    Line::from(spans)
}

/// Build the left-border tag badge text (`#a #b`) that fits in `budget`
/// columns. Takes whole tags greedily; if any don't fit, the leftover count
/// folds into a trailing `+N` (reserving room for it, single-digit since the
/// tag set is capped). Returns `None` when there are no tags or no room.
fn tags_title_text(tags: &[String], budget: usize) -> Option<String> {
    if tags.is_empty() || budget < 3 {
        return None;
    }
    // Each piece carries its own leading space so the run insets from the
    // corner like the priority chip's ` P1 `.
    let pieces: Vec<String> = tags.iter().map(|t| format!(" #{}", t)).collect();
    let mut used = 0usize;
    let mut shown = 0usize;
    for (i, piece) in pieces.iter().enumerate() {
        let more_after = i + 1 < pieces.len();
        // Keep room for a ` +N` marker (≤3 cols) whenever tags would remain.
        let reserve = if more_after { 3 } else { 0 };
        if used + piece.chars().count() + reserve > budget {
            break;
        }
        used += piece.chars().count();
        shown += 1;
    }
    let hidden = tags.len() - shown;
    if shown == 0 {
        // Not even one tag fits beside the priority chip — show just the count.
        return Some(format!(" +{}", tags.len()));
    }
    let mut out: String = pieces[..shown].concat();
    if hidden > 0 {
        out.push_str(&format!(" +{}", hidden));
    }
    Some(out)
}

fn dir_basename(cwd: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_string())
}

/// Greedy word wrap to `width` columns (char-counted). Local copy of the
/// to-do panel's helper — popups' is private and this one doesn't need the
/// continuation-indent variant.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut line_len = 0usize;
    for word in text.split_whitespace() {
        let wlen = word.chars().count();
        if line_len == 0 {
            line = word.to_string();
            line_len = wlen;
        } else if line_len + 1 + wlen <= width {
            line.push(' ');
            line.push_str(word);
            line_len += 1 + wlen;
        } else {
            out.push(std::mem::take(&mut line));
            line = word.to_string();
            line_len = wlen;
        }
        // A single word longer than the width gets hard-truncated rather
        // than overflowing the card.
        if line_len > width {
            line = models::first_line_truncated(&line, width);
            line_len = width;
        }
    }
    out.push(line);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::{TaskPriority, TaskStatus};
    use crate::ui::common::buffer_to_string;
    use crate::ui::palette::BACKLOG_BLUE;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn card(priority: TaskPriority) -> TaskState {
        let mut t = TaskState::new_personal("fix the parser".into());
        t.task_id = "tk-1".into();
        t.priority = priority;
        t.created_at = 1;
        t
    }

    /// Render a To-Do card and return its painted buffer.
    fn render(priority: TaskPriority) -> ratatui::buffer::Buffer {
        render_card(&card(priority))
    }

    /// Render an arbitrary card (wide enough for badges) and return its buffer.
    fn render_card(t: &TaskState) -> ratatui::buffer::Buffer {
        let sessions = HashMap::new();
        let backend = TestBackend::new(28, 5);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                render_task_card(
                    f,
                    f.area(),
                    t,
                    false,
                    TaskStatus::Backlog,
                    BACKLOG_BLUE,
                    &sessions,
                    1_000,
                );
            })
            .expect("render");
        terminal.backend().buffer().clone()
    }

    #[test]
    fn card_shows_priority_badge() {
        let buf = render(TaskPriority::P2);
        assert!(
            buffer_to_string(&buf).contains("P2"),
            "card should show its priority badge:\n{}",
            buffer_to_string(&buf)
        );
    }

    #[test]
    fn card_shows_tags_alongside_priority() {
        let mut t = card(TaskPriority::P1);
        t.tags = vec!["bug".into(), "api".into()];
        let painted = buffer_to_string(&render_card(&t));
        assert!(
            painted.contains("#bug") && painted.contains("#api"),
            "card should show its tag badges:\n{}",
            painted
        );
        // The priority chip must survive alongside the tags.
        assert!(
            painted.contains("P1"),
            "tags must not displace the priority badge:\n{}",
            painted
        );
    }

    #[test]
    fn card_shows_attachment_chip() {
        let mut t = card(TaskPriority::P3);
        t.artifacts.push(crate::orchestrator::Artifact {
            kind: "file".into(),
            path: "/tmp/store/1-doc.md".into(),
            original: "/tmp/doc.md".into(),
            caption: None,
            added_at: 1,
        });
        let painted = buffer_to_string(&render_card(&t));
        // The emoji is double-width, so the test backend inserts a placeholder
        // space cell after it — compare with spaces squeezed out.
        assert!(
            painted.replace(' ', "").contains("📎1"),
            "card should show the attachment chip:\n{}",
            painted
        );
        // A card without attachments shows no chip.
        let bare = buffer_to_string(&render(TaskPriority::P3));
        assert!(!bare.contains("📎"), "no chip expected:\n{}", bare);
    }

    #[test]
    fn tags_title_overflows_to_count_marker() {
        // Plenty of tags but a tiny budget folds the leftovers into `+N`.
        let tags: Vec<String> = vec!["alpha".into(), "bravo".into(), "charlie".into()];
        let text = tags_title_text(&tags, 10).expect("some tags fit");
        assert!(text.contains('+'), "expected overflow marker in {:?}", text);
        // No tags at all, or no room, yields nothing.
        assert_eq!(tags_title_text(&[], 20), None);
        assert_eq!(tags_title_text(&tags, 2), None);
    }

    #[test]
    fn completed_card_renders_usage_and_cost() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut task = card(TaskPriority::P3);
        task.status = TaskStatus::Done;
        task.stats = Some(crate::task_stats::TaskStats {
            input_tokens: 123_000,
            cost_nano_usd: Some(250_000_000),
            estimated: true,
            ..Default::default()
        });
        let mut terminal = Terminal::new(TestBackend::new(44, 5)).unwrap();
        terminal
            .draw(|frame| {
                render_task_card(
                    frame,
                    frame.area(),
                    &task,
                    false,
                    TaskStatus::Done,
                    Color::Green,
                    &HashMap::new(),
                    1_000,
                )
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("123.0K tokens · ~$0.2500"), "{text}");
    }

    fn idle_session(tmux: &str) -> SessionInfo {
        SessionInfo {
            agent_id: "claude".into(),
            agent_kind: crate::agent::AgentKind::Claude,
            pid: 1,
            session_id: tmux.into(),
            cwd: "/tmp".into(),
            project_name: "tmp".into(),
            started_at: 0,
            last_activity: None,
            state: SessionState::Idle,
            last_user_message: None,
            summary: None,
            title: None,
            titling: false,
            model: None,
            git_branch: None,
            version: None,
            jsonl_path: None,
            tmux_session: Some(tmux.into()),
            current_tool: None,
            is_thinking: false,
            context_tokens: None,
            tool_uses_count: 0,
        }
    }

    #[test]
    fn idle_agent_badge_depends_on_card_status() {
        // The same idle session reads differently by phase: a Planning card
        // has a plan waiting, an In Progress card an implementation.
        let session = idle_session("mux-1");
        let sessions: HashMap<&str, &SessionInfo> = [("mux-1", &session)].into();
        let mut t = card(TaskPriority::P3);
        t.tmux = Some("mux-1".into());

        t.status = TaskStatus::Planning;
        let line = meta_line(&t, t.status, &sessions, 1_000).to_string();
        assert!(line.contains("plan ready"), "line: {line}");

        t.status = TaskStatus::Running;
        let line = meta_line(&t, t.status, &sessions, 1_000).to_string();
        assert!(line.contains("idle — check progress"), "line: {line}");
    }

    #[test]
    fn badge_is_color_coded_per_priority() {
        for (p, want) in [
            (TaskPriority::P1, Color::LightRed),
            (TaskPriority::P2, Color::LightYellow),
            (TaskPriority::P3, Color::LightGreen),
            (TaskPriority::P4, Color::LightBlue),
        ] {
            let buf = render(p);
            let area = *buf.area();
            // Find the badge's "P" cell: the hue fills the background, with
            // black text on top.
            let (fg, bg) = (0..area.width)
                .flat_map(|x| (0..area.height).map(move |y| (x, y)))
                .find(|&(x, y)| buf[(x, y)].symbol() == "P")
                .map(|(x, y)| (buf[(x, y)].fg, buf[(x, y)].bg))
                .expect("badge P cell present");
            assert_eq!(bg, want, "{} badge fill", p.label());
            assert_eq!(fg, Color::Black, "{} badge text", p.label());
        }
    }
}

// Unix-only: these construct an App over the on-disk personal store, which
// with_temp_home isolates by redirecting $HOME (unix-only mechanism).
#[cfg(all(test, unix))]
mod task_info_tests {
    use crate::app::App;
    use crate::test_util::with_temp_home;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn info_popup_inlines_excerpt_url_and_footer() {
        with_temp_home(|| {
            let mut app = App::new();
            let id = app.tasks.board.add("write the report").unwrap().unwrap();
            let dir = std::env::temp_dir().join(format!("cchub-info-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let doc = dir.join("notes.md");
            std::fs::write(&doc, "alpha finding\nbeta finding\n").unwrap();
            crate::ops::task::task_artifact_add(
                None,
                &id,
                doc.to_str().unwrap(),
                None,
                Some("research notes".into()),
                false,
            )
            .unwrap();
            crate::ops::task::task_artifact_add(
                None,
                &id,
                "https://example.com/spec",
                None,
                None,
                false,
            )
            .unwrap();
            app.tasks.reload();
            app.focus_task(&id);
            assert!(app.enter_task_info());

            let backend = TestBackend::new(100, 30);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| super::render_task_info(f, f.area(), &mut app))
                .expect("render");
            let dump = buffer_to_string(terminal.backend().buffer());
            assert!(
                dump.contains("write the report"),
                "prompt visible\n{}",
                dump
            );
            assert!(
                dump.contains("alpha finding"),
                "text excerpt inlined\n{}",
                dump
            );
            assert!(
                dump.contains("research notes"),
                "caption on card header\n{}",
                dump
            );
            assert!(
                dump.contains("https://example.com/spec"),
                "url card shows the URL\n{}",
                dump
            );
            assert!(dump.contains("attachment 1/2"), "footer position\n{}", dump);
            std::fs::remove_dir_all(&dir).ok();
        });
    }

    #[test]
    fn info_popup_without_attachments_hints_attach_key() {
        with_temp_home(|| {
            let mut app = App::new();
            let id = app.tasks.board.add("bare card").unwrap().unwrap();
            app.focus_task(&id);
            assert!(app.enter_task_info());
            let backend = TestBackend::new(80, 24);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| super::render_task_info(f, f.area(), &mut app))
                .expect("render");
            let dump = buffer_to_string(terminal.backend().buffer());
            assert!(
                dump.contains("no attachments"),
                "empty state hint expected\n{}",
                dump
            );
        });
    }
}
