//! Builds tab: the recipes down the side, the selected one's page beside
//! them. The page answers "what did it last run, and is it running now?":
//! its border carries the recipe, the state of its build, the resource it
//! holds and its description; inside is the build that matters now
//! (running, else next to run, else the last to finish), then its log. A
//! recipe's older builds are its history, not pages. `f` gives the page the
//! keyboard, to scroll the log.

use crate::app::{App, BuildForm, FormField, LogView, View};
use crate::builds::{recipe, short_commit, Build, BuildStatus};
use crate::models::first_line_truncated;
use crate::ui::common::{age, centered_fixed, popup_block, Dot};
use crate::ui::palette::{ACCENT_BLUE, DIM_TEXT, FAINT_TEXT, LABEL_GRAY, MUTED_TEXT, SEP_GRAY};
use crate::ui::split::{keep_row_in_view, page_title, render_row, SidebarRow, Split};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Padding, Paragraph};
use ratatui::Frame;

/// The progress bar against the route's usual time, in cells.
const BAR_W: usize = 10;

pub(crate) fn hints(app: &App) -> &'static str {
    match app.view {
        crate::app::View::BuildForm => "tab/↑↓:field  ←/→:change  type:edit  enter:run  esc:cancel",
        crate::app::View::BuildLog => {
            "j/k:scroll  PgUp/PgDn:page  G:follow  r:run  n:run with…  c:cancel  esc/F1:list"
        }
        _ => "r:run  n:run with…  c:cancel  enter/f:log  j/k:recipe  tab:next  q:quit",
    }
}

/// What Space does on the selected recipe: release its resource when the tab
/// holds or waits for it, else reserve it. Nothing for a recipe without one.
pub(crate) fn space_verb(app: &App) -> Option<&'static str> {
    let name = app.builds.selected_recipe()?;
    let resource = recipe::named(name)?.resource.as_ref()?;
    Some(
        if app.builds.holds.get(resource).is_some_and(Option::is_some) {
            "release "
        } else {
            "reserve "
        },
    )
}

/// A recipe's dot, in the palette sessions use: white while its build
/// runs, red when the build it shows failed, dark gray otherwise — queued,
/// done, cancelled or never run. Whether the last one worked is the row's
/// `✓`/`✗` to say.
fn build_dot(shown: Option<&Build>) -> Dot {
    match shown.map(|b| b.status) {
        Some(BuildStatus::Running) => Dot::Working,
        Some(BuildStatus::Failed) => Dot::Failed,
        _ => Dot::Resting,
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

fn text(s: impl Into<String>, width: usize, style: Style) -> Span<'static> {
    Span::styled(first_line_truncated(&s.into(), width), style)
}

pub(crate) fn render_builds_body(frame: &mut Frame, area: Rect, app: &mut App) {
    app.render.split = Default::default();
    if area.height < 2 || area.width < 20 {
        return;
    }
    if app.builds.recipes.is_empty() {
        let msg = if app.builds.loaded {
            " No recipes. Add one under [builds.recipes] in config.toml — see `hal help build`."
        } else {
            " Reading builds …"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(DIM_TEXT))),
            area,
        );
        return;
    }

    let now = crate::persist::now_unix_secs();
    let now_ms = crate::ui::now_ms();
    let split = Split::of(area, app.view == View::BuildLog);
    app.render.split.page = split.page;
    render_list(frame, split, app, now_ms);
    if !split.page.is_empty() {
        render_page(frame, split, app, now);
    }
    split.dim_unfocused(frame);
}

fn render_list(frame: &mut Frame, split: Split, app: &mut App, now_ms: u64) {
    let area = split.sidebar;
    app.render.split.sidebar = area;
    keep_row_in_view(
        &mut app.render.builds_scroll,
        app.builds.selected,
        area.height,
    );
    let first = app.render.builds_scroll as usize;
    let shown = app
        .builds
        .recipes
        .iter()
        .enumerate()
        .skip(first)
        .take(area.height as usize);
    for (i, name) in shown {
        let y = area.y + (i - first) as u16;
        let row = sidebar_row(app, name, now_ms);
        render_row(
            frame,
            Rect::new(area.x, y, area.width, 1),
            row,
            (i == app.builds.selected).then(|| split.cursor_bg()),
        );
    }
    let end = app.builds.recipes.len().min(first + area.height as usize);
    app.render.split.rows = (first..end).map(|i| Some((0, i))).collect();
}

/// The state dot and the recipe, white as a session's name is. How its
/// builds went is on its page.
fn sidebar_row(app: &App, name: &str, now_ms: u64) -> SidebarRow {
    SidebarRow {
        mark: build_dot(app.builds.shown(name)).span(now_ms),
        name: name.to_string(),
        name_style: Style::default().fg(Color::White),
    }
}

/// The selected recipe's page: framed with its name, its resource and its
/// description; inside, the build that matters now, each thing said once —
/// its commit, its route and time against the route's usual, its phase or
/// failure, and what comes after it — then that build's log, following its
/// end.
fn render_page(frame: &mut Frame, split: Split, app: &App, now: i64) {
    let Some(name) = app.builds.selected_recipe() else {
        return;
    };
    let recipe = recipe::named(name);
    let shown = app.builds.shown(name);
    let width = split.page.width.saturating_sub(4) as usize;

    let mut block = split
        .page_block(page_title(build_dot(shown).span(crate::ui::now_ms()), name))
        .padding(Padding::horizontal(1));
    if let Some(resource) = recipe.and_then(|r| r.resource.as_deref()) {
        block = block.title(hold_title(app, resource, now));
    }
    if let Some(description) = recipe
        .map(|r| r.description.as_str())
        .filter(|d| !d.is_empty())
    {
        block = block.title_bottom(Span::styled(
            format!(
                " {} ",
                first_line_truncated(description, width.saturating_sub(24))
            ),
            Style::default().fg(DIM_TEXT),
        ));
    }
    if let Some(log) = app.builds.log.as_ref() {
        let follow = match log.back {
            0 => " following ".to_string(),
            n => format!(" {} lines up · G follows ", n),
        };
        block = block.title_bottom(
            Line::from(Span::styled(follow, Style::default().fg(DIM_TEXT))).right_aligned(),
        );
    }
    let inner = block.inner(split.page);
    frame.render_widget(block, split.page);

    let mut lines = match shown {
        Some(build) => {
            let mut lines = vec![commit_line(build, width), time_line(app, build, now)];
            lines.extend(say_line(build, width));
            lines
        }
        None => vec![Line::from(text(
            "never run — r runs it",
            width,
            Style::default().fg(DIM_TEXT),
        ))],
    };
    lines.extend(after_line(app, name, shown, now, width));
    let head_h = (lines.len() as u16).min(inner.height);
    frame.render_widget(
        Paragraph::new(lines),
        Rect::new(inner.x, inner.y, inner.width, head_h),
    );

    let Some(log) = app.builds.log.as_ref() else {
        return;
    };
    // A blank row, the rule, then the log to the bottom.
    let top = head_h + 1;
    if inner.height < top + 2 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(inner.width as usize),
            Style::default().fg(SEP_GRAY),
        )),
        Rect::new(inner.x, inner.y + top, inner.width, 1),
    );
    let body = Rect::new(
        inner.x,
        inner.y + top + 1,
        inner.width,
        inner.height - top - 1,
    );
    frame.render_widget(Paragraph::new(log_lines(log, body)), body);
}

/// The top border's right end: `󰌾 build-box · 12m`, or who is in the way.
fn hold_title(app: &App, resource: &str, now: i64) -> Line<'static> {
    let hold = app.builds.holds.get(resource).cloned().flatten();
    let other = app.builds.probe.holders.get(resource).cloned().flatten();
    let (s, color) = match (hold, other) {
        (Some(h), _) if h.granted => (
            format!("󰌾 {} · {}", resource, age(now, h.since)),
            Color::Green,
        ),
        (Some(h), _) => (
            match h.behind {
                Some(who) => format!("󰔟 {} · behind {}", resource, who),
                None => format!("󰔟 {}", resource),
            },
            Color::Yellow,
        ),
        (None, Some(who)) => (format!("󰌾 {} held by {}", resource, who), Color::Yellow),
        (None, None) => (format!("{} free", resource), DIM_TEXT),
    };
    Line::from(Span::styled(format!(" {} ", s), Style::default().fg(color))).right_aligned()
}

/// `1a2b3c4d5e6 · <subject>`, after the ref when one was pinned; the ref or
/// the working tree until the recipe says what it built. White, as a title
/// is: the dot in the frame says how it went.
fn commit_line(build: &Build, width: usize) -> Line<'static> {
    let parts: Vec<&str> = [
        build.r#ref.as_deref(),
        build.commit.as_deref().map(short_commit),
        build.subject.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect();
    let s = match parts.is_empty() {
        true => build.target().to_string(),
        false => parts.join(" · "),
    };
    Line::from(text(
        s,
        width,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
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
            if let (Some(elapsed), Some(at)) = (build.elapsed(now), build.finished_at) {
                spans.push(Span::styled(
                    format!("{} · {} ago", duration(elapsed), age(now, at)),
                    Style::default().fg(DIM_TEXT),
                ));
            }
        }
    }
    Line::from(spans)
}

/// What the build is doing, or why it did not work. A build that succeeded
/// has nothing to add.
fn say_line(build: &Build, width: usize) -> Option<Line<'static>> {
    let (s, style) = match build.status {
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
        BuildStatus::Succeeded => return None,
    };
    Some(Line::from(text(s, width, style)))
}

/// What comes after it: builds queued behind it; when it did not succeed,
/// the last build that did; or, when `current` reports a commit other than
/// the last success, that commit, since something ran it since.
fn after_line(
    app: &App,
    name: &str,
    shown: Option<&Build>,
    now: i64,
    width: usize,
) -> Option<Line<'static>> {
    let queued = app.builds.queued_behind(name);
    if queued > 0 {
        return Some(Line::from(text(
            format!("+{} queued", queued),
            width,
            Style::default().fg(Color::Yellow),
        )));
    }
    if let Some(current) = app.builds.drifted(name) {
        let mut s = format!("now at {}", short_commit(current));
        let known = app.builds.builds_of(name).find(|b| {
            b.commit
                .as_deref()
                .is_some_and(|c| c.starts_with(current) || current.starts_with(c))
        });
        match known.and_then(|b| b.subject.as_deref()) {
            Some(subject) => s.push_str(&format!(" · {}", subject)),
            None => s.push_str(" · not run from here"),
        }
        return Some(Line::from(text(
            s,
            width,
            Style::default().fg(Color::Yellow),
        )));
    }
    let failed =
        shown.is_some_and(|b| matches!(b.status, BuildStatus::Failed | BuildStatus::Cancelled));
    if failed {
        let last = app.builds.last_success(name)?;
        let mut s = format!(
            "last ✓ {}",
            last.commit
                .as_deref()
                .map(short_commit)
                .unwrap_or(last.target())
        );
        if let Some(subject) = &last.subject {
            s.push_str(&format!(" · {}", subject));
        }
        if let Some(at) = last.finished_at {
            s.push_str(&format!(" · {} ago", age(now, at)));
        }
        return Some(Line::from(text(s, width, Style::default().fg(DIM_TEXT))));
    }
    None
}

// ---- the form ---------------------------------------------------------------

pub(crate) fn render_build_form(frame: &mut Frame, area: Rect, app: &App) {
    let Some(form) = app.builds.form.as_ref() else {
        return;
    };
    let popup = centered_fixed(area, 72, 10);
    frame.render_widget(Clear, popup);
    let block = popup_block(Span::styled(
        " Run with… ",
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
        FormField::Ref if form.r#ref.is_empty() && !focused => recipe::named(&form.recipe)
            .and_then(|r| r.r#ref.clone())
            .unwrap_or_else(|| "working tree".into()),
        FormField::Ref => form.r#ref.clone(),
        FormField::Route => form.route.clone().unwrap_or_else(|| "auto".into()),
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
            } else if line.starts_with("hal: ") {
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

// `App::new()` loads from `$HOME`, which only unix tests can redirect.
#[cfg(all(test, unix))]
mod page_tests {
    use super::*;
    use crate::app::BuildsSnapshot;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        terminal
            .draw(|f| render_builds_body(f, f.area(), app))
            .expect("render");
        buffer_to_string(terminal.backend().buffer())
    }

    // The recipes go down the side; the selected one's page beside them
    // says what it last ran.
    #[test]
    fn the_page_beside_the_list_is_the_selected_recipe() {
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            let mut done = Build::new("build-server", "/tmp", None, None);
            done.status = BuildStatus::Failed;
            done.error = Some("compiler said no".into());
            app.builds.update(BuildsSnapshot {
                recipes: vec!["build-server".into(), "build-client".into()],
                builds: vec![done],
                ..Default::default()
            });
            let out = render(&mut app, 120, 16);
            let rows: Vec<&str> = out.lines().collect();
            assert!(
                rows[0]
                    .chars()
                    .take(32)
                    .collect::<String>()
                    .contains("● build-server"),
                "{out}"
            );
            assert!(
                rows[1]
                    .chars()
                    .take(32)
                    .collect::<String>()
                    .contains("● build-client"),
                "{out}"
            );
            assert!(out.contains("compiler said no"), "{out}");
            assert_eq!(app.render.split.rows, vec![Some((0, 0)), Some((0, 1))]);

            for (w, h) in [(30, 6), (60, 4), (120, 3)] {
                let _ = render(&mut app, w, h);
            }
        });
    }
}
