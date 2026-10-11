//! Split layout: a narrow session list on the left, the selected session's
//! live pane docked on the right — an editor's sidebar and buffer.
//!
//! The sidebar reads like an explorer tree. Each project opens with its
//! name and a horizontal rule to the edge; under it, sessions linked to a task gather beneath that
//! task's header like files in a subfolder, and unlinked sessions follow at
//! the project's own level. A session row is only the dot and the title, so
//! the sidebar stays narrow; branch, model and context go up into the
//! dock's border, for the one session on screen.

use super::{
    badge_color, context_pct, dot_color, hold_header_above, keep_in_view, render_hold_header,
    render_no_sessions, row_title, CardMarks,
};
use crate::app::{App, Dock, View};
use crate::models::{first_line_truncated, ProjectGroup, SessionInfo, SessionState, TaskBadge};
use crate::ui::common::{short_model, STATE_DOT};
use crate::ui::now_ms;
use crate::ui::palette::{
    DIM_TEXT, HANDOFF_ROW_BG, SELECTED_HANDOFF_ROW_BG, SELECTED_ROW_BG, SEP_GRAY,
};
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
/// The task header's mark, the same one the list and the cards use.
const TASK_ICON: &str = "󰢨";

pub(super) fn render_split(frame: &mut Frame, area: Rect, app: &mut App) {
    let sidebar_w = (area.width / 4).clamp(SIDEBAR_MIN, SIDEBAR_MAX);
    if area.width < sidebar_w + 1 + DOCK_MIN {
        app.render.split.dock = Rect::default();
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
    app.render.split.dock = dock;
    render_sidebar(frame, sidebar, app);
    render_dock(frame, dock, app);
    // The side without the keyboard steps back, as an editor's inactive
    // pane does, so where typing goes reads at a glance.
    let unfocused = if app.view == View::Dock {
        sidebar
    } else {
        dock
    };
    frame
        .buffer_mut()
        .set_style(unfocused, Style::default().add_modifier(Modifier::DIM));
}

/// One sidebar row, top to bottom.
enum Row<'a> {
    /// The On hold section's header, above the first held project.
    Hold,
    Project(&'a ProjectGroup),
    /// A task's header, above the sessions linked to it.
    Task(TaskBadge),
    Session {
        group: usize,
        index: usize,
        session: &'a SessionInfo,
        /// Whether the session sits under a task header: its row indents
        /// one step so the dot lines up with the task's title.
        under_task: bool,
    },
    Gap,
}

/// Every row of the sidebar, in order. Task-linked sessions already lead
/// their project, clustered by task (see `App::rebuild_groups`), so one
/// pass that opens a header wherever the task changes builds the tree.
fn rows(app: &App) -> Vec<Row<'_>> {
    let groups = &app.sessions.groups;
    let mut rows = Vec::new();
    for (gi, group) in groups.iter().enumerate() {
        if hold_header_above(groups, gi) > 0 {
            rows.push(Row::Hold);
        }
        rows.push(Row::Project(group));
        let mut open_task: Option<String> = None;
        for (si, session) in group.sessions.iter().enumerate() {
            let badge = app.task_badge(&session.session_id);
            let task_id = badge.as_ref().map(|b| b.task_id.clone());
            if task_id.is_some() && task_id != open_task {
                rows.extend(badge.map(Row::Task));
            }
            let under_task = task_id.is_some();
            open_task = task_id;
            rows.push(Row::Session {
                group: gi,
                index: si,
                session,
                under_task,
            });
        }
        rows.push(Row::Gap);
    }
    rows
}

fn render_sidebar(frame: &mut Frame, area: Rect, app: &mut App) {
    app.render.split.sidebar = area;
    app.render.split.rows.clear();
    if app.sessions.groups.is_empty() {
        render_no_sessions(frame, area);
        return;
    }

    let mut scroll = app.render.grid_scroll;
    let rows = rows(app);
    let (sel_group, sel_in_group) = (app.sessions.sel_group, app.sessions.sel_in_group);
    let selected = rows.iter().position(|r| {
        matches!(r, Row::Session { group, index, .. } if (*group, *index) == (sel_group, sel_in_group))
    });
    // The selected project's first row: its bar, or the On hold header
    // right above it.
    let group_top = rows
        .iter()
        .position(|r| matches!(r, Row::Session { group, .. } if *group == sel_group))
        .and_then(|first| {
            rows[..first]
                .iter()
                .rposition(|r| matches!(r, Row::Project(_)))
        })
        .map(|bar| bar - usize::from(bar > 0 && matches!(rows[bar - 1], Row::Hold)));
    if let (Some(sel), Some(top)) = (selected, group_top) {
        keep_in_view(
            &mut scroll,
            top as u16,
            sel as u16,
            sel as u16 + 1,
            area.height,
        );
    }

    let now = now_ms();
    let visible = rows.iter().skip(scroll as usize).take(area.height as usize);
    for (y, row) in visible.enumerate() {
        let line = Rect::new(area.x, area.y + y as u16, area.width, 1);
        match row {
            Row::Hold => render_hold_header(frame, line, &app.sessions.groups),
            Row::Project(group) => render_project_header(frame, line, group),
            Row::Task(badge) => render_task_header(frame, line, badge),
            Row::Session {
                group,
                index,
                session,
                under_task,
            } => {
                let marks = CardMarks::of(app, *group, *index, session);
                render_session(frame, line, session, *under_task, marks, now);
            }
            Row::Gap => {}
        }
    }
    let hits = rows
        .iter()
        .skip(scroll as usize)
        .take(area.height as usize)
        .map(|r| match r {
            Row::Session { group, index, .. } => Some((*group, *index)),
            _ => None,
        })
        .collect();
    drop(rows);
    app.render.grid_scroll = scroll;
    app.render.split.rows = hits;
}

/// ` 󰉋 name ──────`: the project's name, then a rule to the edge that
/// separates it from the project above.
fn render_project_header(frame: &mut Frame, area: Rect, group: &ProjectGroup) {
    let width = area.width as usize;
    let name = first_line_truncated(&group.name, width.saturating_sub(6));
    let title = format!(" 󰉋 {name} ");
    let rule = "─".repeat(width.saturating_sub(title.chars().count()));
    let line = Line::from(vec![
        Span::styled(
            title,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(rule, Style::default().fg(SEP_GRAY)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// ` 󰢨 task title`, in the task's identity colour (gray once it is done).
fn render_task_header(frame: &mut Frame, area: Rect, badge: &TaskBadge) {
    let title = badge.title.lines().next().unwrap_or("");
    let title = first_line_truncated(title, (area.width as usize).saturating_sub(4));
    let line = Line::from(Span::styled(
        format!(" {TASK_ICON} {title}"),
        Style::default().fg(badge_color(badge)),
    ));
    frame.render_widget(Paragraph::new(line), area);
}

/// ` ● title…`, or `   ● title…` under a task: the indent, the state dot,
/// and the title. The cursor and the handoff mark are the row's
/// background, see [`row_background`].
fn render_session(
    frame: &mut Frame,
    area: Rect,
    session: &SessionInfo,
    under_task: bool,
    marks: CardMarks,
    now: u64,
) {
    let dot = dot_color(&session.state, marks, now);
    let indent = if under_task { "   " } else { " " };
    let mut spans = vec![Span::raw(indent)];
    spans.push(Span::styled(
        format!("{STATE_DOT} "),
        Style::default().fg(dot),
    ));
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let (title, title_style) = row_title(
        session,
        marks,
        (area.width as usize).saturating_sub(used + 1),
    );
    spans.push(Span::styled(title, title_style));

    let mut row = Paragraph::new(Line::from(spans));
    if let Some(bg) = row_background(marks) {
        row = row.style(Style::default().bg(bg));
    }
    frame.render_widget(row, area);
}

/// A session row's background: blue for the handoff mark, lifted under the
/// cursor; gray for the cursor alone; none otherwise.
fn row_background(marks: CardMarks) -> Option<Color> {
    match (marks.selected, marks.handoff) {
        (true, true) => Some(SELECTED_HANDOFF_ROW_BG),
        (false, true) => Some(HANDOFF_ROW_BG),
        (true, false) => Some(SELECTED_ROW_BG),
        (false, false) => None,
    }
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
    let (title, _) = row_title(session, marks, 60);
    Line::from(vec![
        Span::styled(format!(" {STATE_DOT} "), Style::default().fg(dot)),
        Span::styled(
            format!("{title} "),
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

            assert!(rows[0].contains("alpha"), "{}", rows[0]);
            assert!(rows[1].contains("● Fix auth"), "{}", rows[1]);
            // The dock's frame starts right of the sidebar, titled with
            // the selected session, and says why it has no pane.
            let dock: String = rows[0].chars().skip(33).collect();
            assert!(dock.starts_with('╭'), "{}", rows[0]);
            assert!(rows[0].matches("Fix auth").count() == 1, "{}", rows[0]);
            assert!(rows.iter().any(|r| r.contains("outside tmux")), "{rows:#?}");
            // The next attach starts at the dock's inner size.
            assert_eq!(app.render.dock_size, (8, 120 - 32 - 1 - 2));
        });
    }

    #[test]
    fn only_the_selected_name_is_bold() {
        crate::test_util::with_temp_home(|| {
            let session = |id: &str, title: &str| SessionInfo {
                session_id: id.into(),
                title: Some(title.into()),
                state: SessionState::WaitingForInput,
                tmux_session: None,
                ..crate::test_util::session_info()
            };
            let mut app = App::new();
            app.sessions.groups = vec![ProjectGroup {
                name: "alpha".into(),
                cwd: "/tmp/alpha".into(),
                sessions: vec![session("a-1", "Picked"), session("a-2", "Waiting")],
                held: false,
            }];
            let mut terminal = Terminal::new(TestBackend::new(120, 6)).expect("terminal");
            terminal
                .draw(|f| render_split(f, f.area(), &mut app))
                .expect("render");
            let buf = terminal.backend().buffer();
            let bold = |y: u16, first: &str| {
                (0..32)
                    .map(|x| &buf[(x, y)])
                    .find(|c| c.symbol() == first)
                    .expect("title on its row")
                    .modifier
                    .contains(Modifier::BOLD)
            };
            assert!(bold(1, "P"));
            // Needing you is the dot's to say, not the weight's.
            assert!(!bold(2, "W"));
        });
    }

    #[test]
    fn the_handoff_mark_is_the_rows_background() {
        crate::test_util::with_temp_home(|| {
            let session = |id: &str, title: &str| SessionInfo {
                session_id: id.into(),
                title: Some(title.into()),
                tmux_session: None,
                ..crate::test_util::session_info()
            };
            let mut app = App::new();
            app.sessions.groups = vec![ProjectGroup {
                name: "alpha".into(),
                cwd: "/tmp/alpha".into(),
                sessions: vec![session("a-1", "Picked"), session("a-2", "Marked")],
                held: false,
            }];
            app.sessions.handoff = Some(crate::handoff::Handoff {
                session_id: "a-2".into(),
                draft: String::new(),
            });
            let mut terminal = Terminal::new(TestBackend::new(120, 6)).expect("terminal");
            let mut bg = |app: &mut App, y: u16| {
                terminal
                    .draw(|f| render_split(f, f.area(), app))
                    .expect("render");
                terminal.backend().buffer()[(0, y)].bg
            };
            assert_eq!(bg(&mut app, 1), SELECTED_ROW_BG);
            assert_eq!(bg(&mut app, 2), HANDOFF_ROW_BG);
            app.sessions.sel_in_group = 1;
            assert_eq!(bg(&mut app, 1), Color::Reset);
            assert_eq!(bg(&mut app, 2), SELECTED_HANDOFF_ROW_BG);
        });
    }

    #[test]
    fn task_sessions_gather_under_their_task() {
        crate::test_util::with_temp_home(|| {
            let session = |id: &str, title: &str| SessionInfo {
                session_id: id.into(),
                title: Some(title.into()),
                tmux_session: None,
                ..crate::test_util::session_info()
            };
            let mut app = App::new();
            app.sessions.groups = vec![
                ProjectGroup {
                    name: "alpha".into(),
                    cwd: "/tmp/alpha".into(),
                    sessions: vec![
                        session("a-1", "Plan"),
                        session("a-2", "Build"),
                        session("a-3", "Loose"),
                    ],
                    held: false,
                },
                ProjectGroup {
                    name: "beta".into(),
                    cwd: "/tmp/beta".into(),
                    sessions: vec![session("b-1", "Other")],
                    held: false,
                },
            ];
            for id in ["a-1", "a-2"] {
                app.session_task_links.insert(
                    id.into(),
                    crate::tasks::session_links::TaskLink {
                        task_id: "tk-1".into(),
                        title: "Ship it".into(),
                    },
                );
            }
            let rows: Vec<String> = render(&mut app, 120, 9)
                .into_iter()
                .map(|r| {
                    r.chars()
                        .take(32)
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .collect();
            let header = |name: &str| {
                let title = format!(" 󰉋 {name} ");
                let rule = "─".repeat(32 - title.chars().count());
                format!("{title}{rule}")
            };
            assert_eq!(
                rows,
                vec![
                    header("alpha").as_str(),
                    " 󰢨 Ship it",
                    "   ● Plan",
                    "   ● Build",
                    " ● Loose",
                    "",
                    header("beta").as_str(),
                    " ● Other",
                    "",
                ]
            );
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

    // Only the side with the keyboard is drawn at full strength: the dock
    // dims while the list has it, and the list dims while the dock has it.
    #[test]
    fn the_side_without_the_keyboard_is_dimmed() {
        crate::test_util::with_temp_home(|| {
            let mut app = app_with(SessionInfo {
                session_id: "a-1".into(),
                title: Some("Fix auth".into()),
                tmux_session: None,
                ..crate::test_util::session_info()
            });
            let dimmed = |app: &mut App| {
                let mut terminal = Terminal::new(TestBackend::new(120, 6)).expect("terminal");
                terminal
                    .draw(|f| render_split(f, f.area(), app))
                    .expect("render");
                let buf = terminal.backend().buffer();
                let dim = |x: u16| buf[(x, 1)].modifier.contains(Modifier::DIM);
                (dim(3), dim(40))
            };

            assert_eq!(dimmed(&mut app), (false, true));
            app.view = View::Dock;
            assert_eq!(dimmed(&mut app), (true, false));
        });
    }

    // `r` types the new name into the row itself, cursor last; the dock
    // keeps the old name until Enter.
    #[test]
    fn a_rename_is_typed_into_the_row() {
        crate::test_util::with_temp_home(|| {
            let mut app = app_with(SessionInfo {
                session_id: "a-1".into(),
                title: Some("Fix auth".into()),
                tmux_session: None,
                ..crate::test_util::session_info()
            });
            assert!(app.enter_rename_session());
            app.rename_buffer = "Fix login".into();
            let rows = render(&mut app, 120, 10);

            let sidebar: String = rows[1].chars().take(32).collect();
            assert_eq!(sidebar.trim_end(), " ● Fix login▎");
            assert!(rows[0].contains("Fix auth"), "{}", rows[0]);
        });
    }
}
