//! Builds tab: a line per recipe saying what it holds and what its player
//! runs, then a card per build, newest first. A card answers "what is it,
//! where is it, and is it the one I can play on?": the target and commit on
//! its border, the route and time against the route's usual, the phase or why
//! it failed, and an `in player` mark on the build the player was made from.

use crate::app::{App, BuildForm, FormField, LogView};
use crate::builds::{recipe, Build, BuildStatus};
use crate::models::first_line_truncated;
use crate::ui::agents::age;
use crate::ui::common::{centered_fixed, centered_rect, popup_block};
use crate::ui::palette::{ACCENT_BLUE, DIM_TEXT, FAINT_TEXT, LABEL_GRAY, MUTED_TEXT, SEP_GRAY};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};
use ratatui::Frame;

const CARD_W: u16 = 58;
const CARD_H: u16 = 5;
const BAR_W: usize = 10;

pub(crate) fn hints(app: &App) -> &'static str {
    match app.view {
        crate::app::View::BuildForm => {
            "tab/↑↓:field  ←/→:change  type:edit  enter:build  esc:cancel"
        }
        crate::app::View::BuildLog => "j/k:scroll  PgUp/PgDn:page  G:follow  esc:close",
        _ => "n:new  r:rebuild  c:cancel  b:serve  enter/f:log  x:delete  h/j/k/l:nav  tab:next  q:quit",
    }
}

/// What Space does: release what the tab holds or waits for, else reserve.
pub(crate) fn space_verb(app: &App) -> &'static str {
    if app.builds.holds.values().any(Option::is_some) {
        "release "
    } else {
        "reserve "
    }
}

fn status_mark(status: BuildStatus, now_ms: u64) -> (&'static str, Color) {
    match status {
        BuildStatus::Queued => ("󰔟", Color::Yellow),
        BuildStatus::Running => (crate::ui::sessions::spinner_frame(now_ms), Color::Green),
        BuildStatus::Succeeded => ("✓", Color::Green),
        BuildStatus::Failed => ("✗", Color::Red),
        BuildStatus::Cancelled => ("⊘", LABEL_GRAY),
    }
}

/// `42s`, `3m 20s`, `1h 5m`.
fn duration(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{}s", s),
        s if s < 3600 => match s % 60 {
            0 => format!("{}m", s / 60),
            r => format!("{}m {}s", s / 60, r),
        },
        s => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(11)]
}

pub(crate) fn render_builds_body(frame: &mut Frame, area: Rect, app: &mut App) {
    if area.height < 2 || area.width < 20 {
        return;
    }
    let now = crate::builds::now();

    let recipes: Vec<(&str, &recipe::Recipe)> = recipe::all().collect();
    for (row, (name, recipe)) in recipes.iter().enumerate() {
        let y = area.y + row as u16;
        if y >= area.bottom() {
            return;
        }
        let line = recipe_line(app, name, recipe, now);
        frame.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
    }
    let top = recipes.len() as u16 + 1;
    let body = Rect::new(
        area.x,
        area.y + top,
        area.width,
        area.height.saturating_sub(top),
    );

    if app.builds.builds.is_empty() {
        let msg = if app.builds.loaded {
            " No builds yet. n starts one."
        } else {
            " Reading builds …"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(DIM_TEXT))),
            body,
        );
        return;
    }

    let cols = (body.width / CARD_W).max(1);
    app.render.builds_cols = cols;
    let rows_visible = (body.height / CARD_H).max(1);
    let selected_row = app.builds.selected as u16 / cols;
    if selected_row < app.render.builds_scroll {
        app.render.builds_scroll = selected_row;
    } else if selected_row >= app.render.builds_scroll + rows_visible {
        app.render.builds_scroll = selected_row + 1 - rows_visible;
    }

    let cell_w = body.width / cols;
    let now_ms = crate::ui::now_ms();
    for (i, build) in app.builds.builds.iter().enumerate() {
        let (row, col) = (i as u16 / cols, i as u16 % cols);
        if row < app.render.builds_scroll || row >= app.render.builds_scroll + rows_visible {
            continue;
        }
        let y = body.y + (row - app.render.builds_scroll) * CARD_H;
        if y + CARD_H > body.bottom() {
            continue;
        }
        let x = body.x + col * cell_w;
        let w = if col == cols - 1 {
            body.right() - x
        } else {
            cell_w
        };
        let selected = i == app.builds.selected;
        render_card(
            frame,
            Rect::new(x, y, w, CARD_H),
            app,
            build,
            selected,
            now,
            now_ms,
        );
    }
}

/// `build-server  holding build-box · 12m   player 1a2b3c4d5e6`.
fn recipe_line(app: &App, name: &str, recipe: &recipe::Recipe, now: i64) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            format!(" {}", name),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    if let Some(resource) = &recipe.resource {
        let hold = app.builds.holds.get(resource).cloned().flatten();
        let other = app.builds.probe.holders.get(resource).cloned().flatten();
        let (text, color) = match (hold, other) {
            (Some(h), _) if h.granted => (
                format!("󰌾 holding {} · {}", resource, age(now, h.since)),
                Color::Green,
            ),
            (Some(h), _) => (
                match h.behind {
                    Some(who) => format!("󰔟 waiting for {}, held by {}", resource, who),
                    None => format!("󰔟 waiting for {}", resource),
                },
                Color::Yellow,
            ),
            (None, Some(who)) => (format!("󰌾 {} held by {}", resource, who), Color::Yellow),
            (None, None) => (format!("󰍁 {} free", resource), DIM_TEXT),
        };
        spans.push(Span::styled(text, Style::default().fg(color)));
    }
    if let Some(current) = app.builds.probe.current.get(name) {
        spans.push(Span::styled("   player ", Style::default().fg(MUTED_TEXT)));
        spans.push(Span::styled(
            short(current).to_string(),
            Style::default().fg(ACCENT_BLUE),
        ));
    }
    Line::from(spans)
}

fn render_card(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    build: &Build,
    selected: bool,
    now: i64,
    now_ms: u64,
) {
    let (mark, color) = status_mark(build.status, now_ms);
    // The title runs along the border; the body sits a cell in from it.
    let title_w = area.width.saturating_sub(3) as usize;
    let inner_w = area.width.saturating_sub(4) as usize;
    let in_player = app.builds.in_player(build);

    let mut title = format!(" {} {}", mark, build.target());
    if let Some(commit) = &build.commit {
        title.push_str(&format!(" · {}", short(commit)));
    }
    if let Some(subject) = &build.subject {
        title.push_str(&format!(" · {}", subject));
    }
    let title = format!("{} ", first_line_truncated(&title, title_w));

    let border = if selected {
        Color::White
    } else if build.status == BuildStatus::Running {
        Color::Green
    } else {
        SEP_GRAY
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(if selected {
            BorderType::Double
        } else {
            BorderType::Rounded
        })
        .border_style(Style::default().fg(border))
        .padding(Padding::horizontal(1))
        .title(Span::styled(
            title,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    if in_player {
        block = block.title_bottom(
            Line::from(Span::styled(
                " ● in player ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ))
            .right_aligned(),
        );
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = vec![
        time_line(app, build, now),
        say_line(build, in_player, now, inner_w),
        where_line(build, now, inner_w),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The route, then how long: against the route's usual while running.
fn time_line(app: &App, build: &Build, now: i64) -> Line<'static> {
    let route = build
        .taken
        .clone()
        .or_else(|| build.route.clone())
        .unwrap_or_else(|| "auto".into());
    let mut spans = vec![
        Span::styled(route, Style::default().fg(FAINT_TEXT)),
        Span::raw("  "),
    ];
    match build.status {
        BuildStatus::Queued => spans.push(Span::styled(
            format!("queued {}", age(now, build.created_at)),
            Style::default().fg(DIM_TEXT),
        )),
        BuildStatus::Running => {
            let elapsed = build.elapsed(now).unwrap_or(0);
            match app.builds.typical(build) {
                Some(usual) if usual > 0 => {
                    let filled = ((elapsed as f64 / usual as f64) * BAR_W as f64)
                        .round()
                        .clamp(0.0, (BAR_W - 1) as f64) as usize;
                    let over = elapsed > usual;
                    spans.push(Span::styled(
                        "▰".repeat(filled),
                        Style::default().fg(if over { Color::Yellow } else { Color::Green }),
                    ));
                    spans.push(Span::styled(
                        "▱".repeat(BAR_W - filled),
                        Style::default().fg(SEP_GRAY),
                    ));
                    spans.push(Span::styled(
                        format!("  {} / ~{}", duration(elapsed), duration(usual)),
                        Style::default().fg(FAINT_TEXT),
                    ));
                }
                _ => spans.push(Span::styled(
                    duration(elapsed),
                    Style::default().fg(FAINT_TEXT),
                )),
            }
        }
        _ => {
            if let Some(elapsed) = build.elapsed(now) {
                spans.push(Span::styled(
                    duration(elapsed),
                    Style::default().fg(DIM_TEXT),
                ));
            }
        }
    }
    Line::from(spans)
}

/// What the build has to say: what it is doing, why it failed, or where it
/// stands with the player.
fn say_line(build: &Build, in_player: bool, now: i64, width: usize) -> Line<'static> {
    let (text, style) = match build.status {
        BuildStatus::Queued | BuildStatus::Running => (
            build.phase.clone().unwrap_or_else(|| "starting".into()),
            Style::default().fg(Color::White),
        ),
        BuildStatus::Failed => (
            build
                .error
                .clone()
                .or_else(|| build.exit_code.map(|c| format!("exited with {}", c)))
                .unwrap_or_else(|| "failed".into()),
            Style::default().fg(Color::Red),
        ),
        BuildStatus::Cancelled => ("cancelled".into(), Style::default().fg(LABEL_GRAY)),
        BuildStatus::Succeeded => match build.served_at {
            Some(at) if in_player => (
                format!("served {} ago", age(now, at)),
                Style::default().fg(Color::Green),
            ),
            _ if in_player => (
                "built — b serves it".into(),
                Style::default().fg(FAINT_TEXT),
            ),
            Some(at) => (
                format!("served {} ago, since replaced", age(now, at)),
                Style::default().fg(DIM_TEXT),
            ),
            None => ("built".into(), Style::default().fg(DIM_TEXT)),
        },
    };
    Line::from(Span::styled(first_line_truncated(&text, width), style))
}

/// The checkout, and how long ago the build was asked for.
fn where_line(build: &Build, now: i64, width: usize) -> Line<'static> {
    let checkout = std::path::Path::new(&build.cwd)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| build.cwd.clone());
    let text = format!("{} · {} ago", checkout, age(now, build.created_at));
    Line::from(Span::styled(
        first_line_truncated(&text, width),
        Style::default().fg(DIM_TEXT),
    ))
}

// ---- the form ---------------------------------------------------------------

pub(crate) fn render_build_form(frame: &mut Frame, area: Rect, app: &App) {
    let Some(form) = app.builds.form.as_ref() else {
        return;
    };
    let popup = centered_fixed(area, 72, 11);
    frame.render_widget(Clear, popup);
    let block = popup_block(Span::styled(
        " New build ",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines: Vec<Line> = FormField::ALL
        .iter()
        .map(|field| field_line(form, *field, inner.width as usize))
        .collect();
    lines.push(Line::raw(""));
    if let Some(recipe) = recipe::named(&form.recipe) {
        lines.push(Line::from(Span::styled(
            format!(
                " {}",
                first_line_truncated(&recipe.description, inner.width as usize - 2)
            ),
            Style::default().fg(DIM_TEXT),
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn field_line(form: &BuildForm, field: FormField, width: usize) -> Line<'static> {
    let focused = form.field == field;
    let value = match field {
        FormField::Recipe => form.recipe.clone(),
        FormField::Checkout if focused => form.cwd.clone(),
        FormField::Checkout => tilde(&form.cwd),
        FormField::Ref if form.r#ref.is_empty() && !focused => "working tree".into(),
        FormField::Ref => form.r#ref.clone(),
        FormField::Route => form.route.clone().unwrap_or_else(|| "auto".into()),
        FormField::Serve => if form.serve { "yes, once built" } else { "no" }.into(),
    };
    let value = if field.is_text() {
        let cursor = if focused { "▎" } else { "" };
        format!("{}{}", value, cursor)
    } else if focused {
        format!("‹ {} ›", value)
    } else {
        value
    };
    let (marker, label_style, value_style) = if focused {
        (
            "▸",
            Style::default()
                .fg(ACCENT_BLUE)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(Color::White),
        )
    } else {
        (
            " ",
            Style::default().fg(LABEL_GRAY),
            Style::default().fg(FAINT_TEXT),
        )
    };
    let placeholder = field == FormField::Ref && form.r#ref.is_empty() && !focused;
    let value_style = if placeholder {
        value_style.fg(DIM_TEXT).add_modifier(Modifier::ITALIC)
    } else {
        value_style
    };
    Line::from(vec![
        Span::styled(format!(" {} ", marker), label_style),
        Span::styled(format!("{:<9}", field.label()), label_style),
        Span::styled(
            first_line_truncated(&value, width.saturating_sub(13)),
            value_style,
        ),
    ])
}

/// A path under the home directory, written the way a person types it.
fn tilde(path: &str) -> String {
    let home = dirs::home_dir().map(|h| h.display().to_string());
    match home.as_deref().and_then(|h| path.strip_prefix(h)) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{}", rest),
        _ => path.to_string(),
    }
}

// ---- the log ----------------------------------------------------------------

pub(crate) fn render_build_log(frame: &mut Frame, area: Rect, app: &App) {
    let Some(log) = app.builds.log.as_ref() else {
        return;
    };
    let build = app.builds.builds.iter().find(|b| b.id == log.id);
    let popup = centered_rect(area, 0.92);
    frame.render_widget(Clear, popup);
    let title = match build {
        Some(b) => format!(" {} · {} ", b.target(), b.status.label()),
        None => format!(" {} ", log.id),
    };
    let follow = if log.back == 0 {
        " following ".to_string()
    } else {
        format!(" {} lines up · G follows ", log.back)
    };
    let block = popup_block(Span::styled(
        title,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Line::from(Span::styled(follow, Style::default().fg(DIM_TEXT))).right_aligned());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(Paragraph::new(log_lines(log, inner)), inner);
}

fn log_lines(log: &LogView, area: Rect) -> Vec<Line<'static>> {
    if log.lines.is_empty() {
        return vec![Line::from(Span::styled(
            " nothing written yet",
            Style::default().fg(DIM_TEXT),
        ))];
    }
    let end = log.lines.len() - log.back;
    let start = end.saturating_sub(area.height as usize);
    let width = area.width as usize;
    log.lines[start..end]
        .iter()
        .map(|line| {
            let style = if line.starts_with("$ ") {
                Style::default()
                    .fg(ACCENT_BLUE)
                    .add_modifier(Modifier::BOLD)
            } else if line.starts_with("cc-hub: ") {
                Style::default().fg(MUTED_TEXT)
            } else {
                Style::default().fg(FAINT_TEXT)
            };
            Line::from(Span::styled(first_line_truncated(line, width), style))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_like_a_person_says_them() {
        assert_eq!(duration(42), "42s");
        assert_eq!(duration(120), "2m");
        assert_eq!(duration(200), "3m 20s");
        assert_eq!(duration(3900), "1h 5m");
    }
}
