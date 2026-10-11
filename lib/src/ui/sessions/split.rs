//! Split layout: a narrow session list on the left, the selected session's
//! live pane docked on the right — an editor's sidebar and buffer.
//!
//! The sidebar keeps the list layout's grouping (project headers, the On
//! hold section) but only the dot, the title and the age, so it stays
//! narrow. What the rows drop — branch, model, context — goes up into the
//! dock's border, for the one session on screen.

use super::{
    context_pct, dot_color, hold_header_above, keep_in_view, render_hold_header,
    render_no_sessions, row_title, CardMarks, GROUP_GAP, GROUP_HEADER_HEIGHT,
};
use crate::app::{App, Dock, View};
use crate::models::{first_line_truncated, ProjectGroup, SessionInfo, SessionState};
use crate::ui::common::{format_elapsed, selection_stripe, short_model, STATE_DOT};
use crate::ui::now_ms;
use crate::ui::palette::{DIM_TEXT, HANDOFF_BLUE, ICE_BLUE, SELECTED_ROW_BG, SEP_GRAY};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;
use tui_term::widget::{Cursor, PseudoTerminal};

/// The sidebar takes a quarter of the width, within these bounds.
const SIDEBAR_MIN: u16 = 32;
const SIDEBAR_MAX: u16 = 48;
/// Narrower than this, the dock isn't worth drawing: the sidebar takes it
/// all.
const DOCK_MIN: u16 = 40;
/// Stripe(1) + dot(1) + space(1).
const ROW_LEFT: usize = 3;

pub(super) fn render_split(frame: &mut Frame, area: Rect, app: &mut App) {
    let sidebar_w = (area.width / 4).clamp(SIDEBAR_MIN, SIDEBAR_MAX);
    if area.width < sidebar_w + 1 + DOCK_MIN {
        render_sidebar(frame, area, app);
        return;
    }
    let sidebar = Rect::new(area.x, area.y, sidebar_w, area.height);
    let dock = Rect::new(
        area.x + sidebar_w + 1,
        area.y,
        area.width - sidebar_w - 1,
        area.height,
    );
    render_sidebar(frame, sidebar, app);
    render_dock(frame, dock, app);
}

fn render_sidebar(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.sessions.groups.is_empty() {
        render_no_sessions(frame, area);
        return;
    }

    // Content-space y of each group's header: the On hold header above the
    // group that opens that section, one row per session, then the gap.
    let mut group_offsets: Vec<u16> = Vec::new();
    let mut y_acc: u16 = 0;
    for (gi, group) in app.sessions.groups.iter().enumerate() {
        y_acc = y_acc.saturating_add(hold_header_above(&app.sessions.groups, gi));
        group_offsets.push(y_acc);
        y_acc = y_acc.saturating_add(GROUP_HEADER_HEIGHT + group.sessions.len() as u16 + GROUP_GAP);
    }

    let (sel_group, sel_in_group) = (app.sessions.sel_group, app.sessions.sel_in_group);
    let g_offset = group_offsets[sel_group];
    let row_y = g_offset + GROUP_HEADER_HEIGHT + sel_in_group as u16;
    keep_in_view(
        &mut app.render.grid_scroll,
        g_offset - hold_header_above(&app.sessions.groups, sel_group),
        row_y,
        row_y + 1,
        area.height,
    );

    let scroll = app.render.grid_scroll as i32;
    let on_screen = |y: u16| {
        let sy = y as i32 - scroll;
        (sy >= 0 && sy < area.height as i32)
            .then(|| Rect::new(area.x, area.y + sy as u16, area.width, 1))
    };
    let now = now_ms();
    let groups = &app.sessions.groups;
    for (gi, group) in groups.iter().enumerate() {
        let g_y = group_offsets[gi];
        let above = hold_header_above(groups, gi);
        if let Some(row) = (above > 0).then(|| on_screen(g_y - above)).flatten() {
            render_hold_header(frame, row, groups);
        }
        if let Some(row) = on_screen(g_y) {
            render_group_header(frame, row, group);
        }
        for (si, session) in group.sessions.iter().enumerate() {
            let Some(row) = on_screen(g_y + GROUP_HEADER_HEIGHT + si as u16) else {
                continue;
            };
            let marks = CardMarks::of(app, gi, si, session);
            render_row(frame, row, session, marks, now);
        }
    }
}

/// One-row project header: the name, and the session count at the right
/// edge.
fn render_group_header(frame: &mut Frame, area: Rect, group: &ProjectGroup) {
    let count = format!("{} ", group.sessions.len());
    let name_w = (area.width as usize).saturating_sub(count.chars().count() + 1);
    let name = format!(
        " {}",
        first_line_truncated(&group.name, name_w.saturating_sub(1))
    );
    let pad = (area.width as usize).saturating_sub(name.chars().count() + count.chars().count());
    let line = Line::from(vec![
        Span::styled(
            name,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(pad)),
        Span::styled(count, Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// `▌● title…  12m`: the cursor stripe, the state dot, the title, and the
/// last activity's age — ice blue once the prompt cache went cold.
fn render_row(frame: &mut Frame, area: Rect, session: &SessionInfo, marks: CardMarks, now: u64) {
    let dot = dot_color(&session.state, marks, now);
    let age = session
        .last_activity
        .map(|ts| format!(" {} ", format_elapsed(now, ts)))
        .unwrap_or_default();
    let age_color = if session.cache_cold(now) {
        ICE_BLUE
    } else {
        Color::DarkGray
    };
    let budget = (area.width as usize).saturating_sub(ROW_LEFT + age.chars().count());
    let (title, title_style) = row_title(session, dot);
    let title = first_line_truncated(&title, budget.saturating_sub(1));
    let pad = budget.saturating_sub(title.chars().count());

    let stripe = if marks.handoff {
        Span::styled("▌", Style::default().fg(HANDOFF_BLUE))
    } else {
        selection_stripe(marks.selected)
    };
    let line = Line::from(vec![
        stripe,
        Span::styled(format!("{STATE_DOT} "), Style::default().fg(dot)),
        Span::styled(title, title_style),
        Span::raw(" ".repeat(pad)),
        Span::styled(age, Style::default().fg(age_color)),
    ]);
    let mut row = Paragraph::new(line);
    if marks.selected {
        row = row.style(Style::default().bg(SELECTED_ROW_BG));
    }
    frame.render_widget(row, area);
}

/// The dock: a rounded frame titled with the selected session, around its
/// live pane — or, when there is no pane to show, a word on why. The frame
/// lights up while the keyboard is inside.
fn render_dock(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.view == View::Dock;
    let session = app.selected_session_info().cloned();
    let now = now_ms();

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused { Color::White } else { SEP_GRAY }));
    if let Some(s) = session.as_ref() {
        let marks = CardMarks {
            held: app.sessions.holds.contains(&s.session_id),
            ..CardMarks::default()
        };
        block = block
            .title(dock_title(s, marks, now))
            .title(dock_facts(s).alignment(Alignment::Right));
    }
    let live = app.sessions.dock.pane().is_some();
    let hint = match (focused, live) {
        (true, _) => Some(" F1 back to the list "),
        (false, true) => Some(" Enter type here "),
        (false, false) => None,
    };
    if let Some(hint) = hint {
        block = block.title_bottom(
            Line::from(Span::styled(hint, Style::default().fg(DIM_TEXT)))
                .alignment(Alignment::Right),
        );
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    app.render.dock_size = (inner.height, inner.width);

    let Some(pane) = app.sessions.dock.pane_mut() else {
        let words = placeholder(session.as_ref(), &app.sessions.dock);
        render_placeholder(frame, inner, &words);
        return;
    };
    pane.resize(inner.height, inner.width);
    pane.set_viewport_origin(inner.x, inner.y);
    let Ok(parser) = pane.parser.lock() else {
        return;
    };
    // Only the focused pane shows a cursor: it marks where typing goes.
    let mut cursor = Cursor::default();
    if !focused {
        cursor.hide();
    }
    frame.render_widget(PseudoTerminal::new(parser.screen()).cursor(cursor), inner);
}

/// ` ● title ` in the session's own colours.
fn dock_title(session: &SessionInfo, marks: CardMarks, now: u64) -> Line<'static> {
    let dot = dot_color(&session.state, marks, now);
    let (title, _) = row_title(session, dot);
    Line::from(vec![
        Span::styled(format!(" {STATE_DOT} "), Style::default().fg(dot)),
        Span::styled(
            format!("{} ", first_line_truncated(&title, 60)),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

/// ` branch · model · 42% `: what the sidebar rows leave out.
fn dock_facts(session: &SessionInfo) -> Line<'static> {
    let mut facts: Vec<String> = Vec::new();
    if let Some(branch) = session.git_branch.as_deref().filter(|b| !b.is_empty()) {
        facts.push(first_line_truncated(branch, 32));
    }
    if let Some(model) = session.model.as_deref().filter(|m| !m.is_empty()) {
        facts.push(short_model(model).to_string());
    }
    if let Some((pct, _)) = context_pct(session) {
        facts.push(format!("{pct:.0}%"));
    }
    if facts.is_empty() {
        return Line::default();
    }
    Line::from(Span::styled(
        format!(" {} ", facts.join(" · ")),
        Style::default().fg(Color::DarkGray),
    ))
}

/// Why the dock has no pane, and what Enter does about it.
fn placeholder(session: Option<&SessionInfo>, dock: &Dock) -> [String; 2] {
    let Some(session) = session else {
        return ["No session selected.".into(), String::new()];
    };
    if session.state == SessionState::Inactive {
        return ["Not running.".into(), "Enter resumes it.".into()];
    }
    if session.tmux_session.is_none() {
        return [
            "Running in its own terminal window, outside tmux.".into(),
            "Enter brings that window forward.".into(),
        ];
    }
    match dock.failure() {
        Some(e) => ["Couldn't attach.".into(), e.to_string()],
        None => ["Attaching…".into(), String::new()],
    }
}

/// The placeholder's two lines, centred in the dock.
fn render_placeholder(frame: &mut Frame, area: Rect, words: &[String; 2]) {
    let lines = vec![
        Line::from(Span::styled(
            words[0].clone(),
            Style::default().fg(Color::Gray),
        )),
        Line::from(Span::styled(
            words[1].clone(),
            Style::default().fg(DIM_TEXT),
        )),
    ];
    let top = area.height.saturating_sub(lines.len() as u16) / 2;
    let body = Rect::new(area.x, area.y + top, area.width, area.height - top);
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), body);
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::models::ProjectGroup;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(app: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        terminal
            .draw(|f| render_split(f, f.area(), app))
            .expect("render");
        let buf = terminal.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    fn app_with(session: SessionInfo) -> App {
        let mut app = App::new();
        app.sessions.groups = vec![ProjectGroup {
            name: "alpha".into(),
            cwd: "/tmp/alpha".into(),
            sessions: vec![session],
            held: false,
        }];
        app
    }

    #[test]
    fn sidebar_rows_sit_beside_the_dock() {
        crate::test_util::with_temp_home(|| {
            let mut app = app_with(SessionInfo {
                session_id: "a-1".into(),
                title: Some("Fix auth".into()),
                state: SessionState::WaitingForInput,
                tmux_session: None,
                ..crate::test_util::session_info()
            });
            let rows = render(&mut app, 120, 10);

            assert!(rows[0].starts_with(" alpha"), "{}", rows[0]);
            assert!(rows[1].contains("● Fix auth"), "{}", rows[1]);
            // The dock's frame starts right of the sidebar, titled with
            // the selected session, and says why it has no pane.
            assert!(rows[0][32..].contains("╭"), "{}", rows[0]);
            assert!(rows[0].matches("Fix auth").count() == 1, "{}", rows[0]);
            assert!(rows.iter().any(|r| r.contains("outside tmux")), "{rows:#?}");
            // The next attach starts at the dock's inner size.
            assert_eq!(app.render.dock_size, (8, 120 - 32 - 1 - 2));
        });
    }

    #[test]
    fn a_narrow_terminal_drops_the_dock() {
        crate::test_util::with_temp_home(|| {
            let mut app = app_with(SessionInfo {
                session_id: "a-1".into(),
                title: Some("Fix auth".into()),
                ..crate::test_util::session_info()
            });
            let rows = render(&mut app, 60, 6);
            assert!(rows.iter().all(|r| !r.contains("╭")), "{rows:#?}");
        });
    }
}
