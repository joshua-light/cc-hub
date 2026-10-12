//! Type-to-filter pickers sharing one chrome: a filter row with a match
//! count, then rows with the matched chars highlighted.

use super::widgets::{highlight_spans, render_filter_row, RowStyles};
use crate::app::{App, FinderLine, PickerRow, SessionFinderChoice};
use crate::models::{first_line_truncated, SessionState};
use crate::ui::common::{centered_fixed, popup_block, task_color, Dot, COL_SEP, STATE_DOT};
use crate::ui::palette::{DIM_TEXT, LABEL_GRAY, MUTED_TEXT, SEP_GRAY};
use chrono::{Local, NaiveDate, TimeZone};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;
use std::collections::HashMap;

/// `N` on the Sessions tab: fuzzy-filter the model for the selected coding
/// agent. The footer shows the agent and spawn cwd captured by the picker.
pub(crate) fn render_model_picker(frame: &mut Frame, area: Rect, app: &App) {
    let Some(picker) = app.model_picker.as_ref() else {
        return;
    };
    let choices = &picker.choices;

    let desired_w = 60u16.min(area.width);
    let desired_h =
        (crate::agent::DEFAULT_CLAUDE_MODELS.len().max(choices.len()) as u16 + 5).min(area.height);
    let popup = centered_fixed(area, desired_w, desired_h);
    frame.render_widget(Clear, popup);

    let block = popup_block(Span::styled(
        " New session — pick model / agent ",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        format!(" → {} in {} ", picker.agent_id, picker.cwd),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height < 3 || inner.width == 0 {
        return;
    }

    render_filter_row(
        frame,
        inner,
        &picker.filter,
        format!("{}/{} ", picker.rows.len(), choices.len()),
    );

    let list_area = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 2);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if picker.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no matches — backspace to widen)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let visible = list_area.height as usize;
        let start = picker.selected.saturating_sub(visible.saturating_sub(1));
        let label_width = choices
            .iter()
            .map(|choice| choice.label.chars().count())
            .max()
            .unwrap_or(0);
        for (i, row) in picker.rows.iter().enumerate().skip(start).take(visible) {
            let Some(choice) = choices.get(row.choice) else {
                continue;
            };
            let label = &choice.label;
            let detail = &choice.detail;
            let selected = i == picker.selected;
            let styles = RowStyles::new(selected, Color::Gray);
            let bar = styles.bar;
            let mut spans = vec![styles.cursor(selected)];
            spans.extend(highlight_spans(
                label,
                &row.label_indices,
                styles.label,
                styles.label_hl,
            ));
            spans.push(Span::styled(
                " ".repeat(label_width.saturating_sub(label.chars().count()) + 2),
                bar,
            ));
            spans.extend(highlight_spans(
                detail,
                &row.detail_indices,
                styles.detail,
                styles.detail_hl,
            ));
            lines.push(Line::from(spans));
        }
    }

    frame.render_widget(Paragraph::new(lines), list_area);
}

/// `L` on the Sessions tab: fuzzy-filter which task the selected session is
/// linked to. Same chrome and interaction as the model picker; the footer
/// names the session being linked.
pub(crate) fn render_task_link_picker(frame: &mut Frame, area: Rect, app: &App) {
    let Some(picker) = app.task_link_picker.as_ref() else {
        return;
    };
    let choices = &picker.choices;

    // Wide enough for a 48-char title column plus the longest
    // `icon board-label` status chip without truncating it.
    let desired_w = 72u16.min(area.width);
    let desired_h = (choices.len().max(8) as u16 + 5).min(area.height);
    let popup = centered_fixed(area, desired_w, desired_h);
    frame.render_widget(Clear, popup);

    let block = popup_block(Span::styled(
        " Link session → task ",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        format!(" → {} ", picker.session_label),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height < 3 || inner.width == 0 {
        return;
    }

    render_filter_row(
        frame,
        inner,
        &picker.filter,
        format!("{}/{} ", picker.rows.len(), choices.len()),
    );

    let list_area = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 2);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if picker.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no matches — backspace to widen)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let visible = list_area.height as usize;
        let start = picker.selected.saturating_sub(visible.saturating_sub(1));
        let label_width = choices
            .iter()
            .map(|choice| choice.label.chars().count())
            .max()
            .unwrap_or(0);
        for (i, row) in picker.rows.iter().enumerate().skip(start).take(visible) {
            let Some(choice) = choices.get(row.choice) else {
                continue;
            };
            let label = &choice.label;
            let detail = &choice.detail;
            let selected = i == picker.selected;
            let unlink = choice.action == crate::app::TaskLinkAction::Unlink;
            let mut styles = RowStyles::new(selected, Color::Gray);
            if unlink && !selected {
                // The destructive row reads differently at a glance.
                styles.label = Style::default().fg(Color::Red);
                styles.label_hl = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
                styles.detail_hl = Style::default().fg(Color::Red);
            }
            let bar = styles.bar;
            let mut spans = vec![styles.cursor(selected)];
            spans.extend(highlight_spans(
                label,
                &row.label_indices,
                styles.label,
                styles.label_hl,
            ));
            spans.push(Span::styled(
                " ".repeat(label_width.saturating_sub(label.chars().count()) + 2),
                bar,
            ));
            match choice.status {
                // Task rows end their detail with the status's board label
                // (see `task_link_candidate`); render that tail as a colored
                // status chip — same hue as the Tasks-board column — with an
                // icon the fuzzy filter never sees.
                Some(status) => {
                    let (icon, accent) = crate::ui::common::task_status_meta(status);
                    let status_label = status.board_label();
                    let prefix_chars = detail
                        .chars()
                        .count()
                        .saturating_sub(status_label.chars().count());
                    let prefix: String = detail.chars().take(prefix_chars).collect();
                    let prefix_idx: Vec<usize> = row
                        .detail_indices
                        .iter()
                        .copied()
                        .filter(|i| *i < prefix_chars)
                        .collect();
                    let status_idx: Vec<usize> = row
                        .detail_indices
                        .iter()
                        .copied()
                        .filter(|i| *i >= prefix_chars)
                        .map(|i| i - prefix_chars)
                        .collect();
                    // On the white selection bar the light status hues wash
                    // out — readability wins there; the bar itself already
                    // marks the row.
                    let status_base = if selected {
                        bar.fg(Color::Black).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(accent)
                    };
                    spans.extend(highlight_spans(
                        &prefix,
                        &prefix_idx,
                        styles.detail,
                        styles.detail_hl,
                    ));
                    spans.push(Span::styled(format!("{} ", icon), status_base));
                    spans.extend(highlight_spans(
                        status_label,
                        &status_idx,
                        status_base,
                        styles.detail_hl,
                    ));
                }
                None => spans.extend(highlight_spans(
                    detail,
                    &row.detail_indices,
                    styles.detail,
                    styles.detail_hl,
                )),
            }
            lines.push(Line::from(spans));
        }
    }

    frame.render_widget(Paragraph::new(lines), list_area);
}

/// `/` on the Sessions tab: fuzzy-search the whole session archive. Each row
/// is a table line — live dot, name, project, linked task, age — so the eye
/// can run down any one column; unfiltered, the rows sit under a heading per
/// [`Period`]. A session never named shows its first message, dimmed: that is
/// what the agent was told, not what the user called it.
pub(crate) fn render_session_finder(frame: &mut Frame, area: Rect, app: &App) {
    let Some(finder) = app.session_finder.as_ref() else {
        return;
    };
    let choices = &finder.choices;
    let now = crate::ui::now_ms();

    let desired_w = 120u16.min(area.width);
    let desired_h = area.height.saturating_sub(4).max(12).min(area.height);
    let popup = centered_fixed(area, desired_w, desired_h);
    frame.render_widget(Clear, popup);

    let block = popup_block(Span::styled(
        " Find session ",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        " title · first message · project · task · session id ",
        Style::default().fg(DIM_TEXT),
    ));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height < 3 || inner.width == 0 {
        return;
    }

    render_filter_row(
        frame,
        inner,
        &finder.filter,
        if finder.loading {
            "indexing… ".to_string()
        } else {
            format!("{}/{} ", finder.rows.len(), choices.len())
        },
    );

    let list_area = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 2);
    if finder.rows.is_empty() {
        let empty = Line::from(Span::styled(
            if finder.loading {
                "  (indexing the session archive…)"
            } else {
                "  (no matches — backspace to widen)"
            },
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(empty), list_area);
        return;
    }

    let today = Local::now().date_naive();
    let lines = finder.lines(today, local_day);
    let visible = list_area.height as usize;
    let selected_line = lines
        .iter()
        .position(|line| *line == FinderLine::Row(finder.selected))
        .unwrap_or(0);
    let start = selected_line.saturating_sub(visible.saturating_sub(1));

    // Sized over every row that passed the filter, not only those on
    // screen, so the columns hold still while scrolling.
    let shown = || finder.rows.iter().filter_map(|row| choices.get(row.choice));
    let columns = FinderColumns {
        width: list_area.width as usize,
        project: shown()
            .map(|c| c.project.chars().count())
            .max()
            .unwrap_or(0)
            .min(FINDER_PROJECT_MAX),
        task: shown()
            .filter_map(|c| c.task.as_ref())
            .map(|t| t.title.lines().next().unwrap_or("").chars().count() + 2)
            .max()
            .map(|w| w.min(FINDER_TASK_MAX)),
    };
    let live: HashMap<&str, &SessionState> = app
        .sessions
        .last_sessions
        .iter()
        .filter(|s| s.state != SessionState::Inactive)
        .map(|s| (s.session_id.as_str(), &s.state))
        .collect();

    let mut out: Vec<Line<'static>> = Vec::new();
    for line in lines.iter().skip(start).take(visible) {
        match *line {
            FinderLine::Period(period, count) => {
                out.push(period_heading(&period.label(today), count, columns.width))
            }
            FinderLine::Row(i) => {
                let row = &finder.rows[i];
                let Some(choice) = choices.get(row.choice) else {
                    continue;
                };
                let state = live.get(choice.session_id.as_str()).copied();
                out.push(finder_row(
                    choice,
                    row,
                    state,
                    &columns,
                    i == finder.selected,
                    now,
                ));
            }
        }
    }
    frame.render_widget(Paragraph::new(out), list_area);
}

/// The project column never grows past this; longer names truncate.
const FINDER_PROJECT_MAX: usize = 18;
/// Linked-task column cap: icon + 28 chars of task title.
const FINDER_TASK_MAX: usize = 30;
/// Age column: "59m", "23h", "364d".
const FINDER_AGE_W: usize = 4;
/// Cursor marker(2) + live dot(2).
const FINDER_LEFT: usize = 4;

/// Advance widths of the finder's table, decided once per frame.
struct FinderColumns {
    width: usize,
    project: usize,
    /// `None` when no row is linked to a task: the column isn't drawn.
    task: Option<usize>,
}

impl FinderColumns {
    /// What's left for the name once every fixed column has its share.
    fn label(&self) -> usize {
        let task = self.task.map_or(0, |w| w + COL_SEP);
        self.width
            .saturating_sub(FINDER_LEFT + self.project + COL_SEP + task + COL_SEP + FINDER_AGE_W)
    }
}

/// The calendar day of an epoch-ms instant, in local time.
fn local_day(ms: u64) -> NaiveDate {
    Local
        .timestamp_millis_opt(ms as i64)
        .single()
        .map(|t| t.date_naive())
        .unwrap_or_default()
}

/// ` Today  12 ───…`: the same heading-and-rule shape as the grid's On hold
/// section.
fn period_heading(label: &str, count: usize, width: usize) -> Line<'static> {
    let title = format!(" {label} ");
    let count = format!(" {count} ");
    let used = title.chars().count() + count.chars().count();
    Line::from(vec![
        Span::styled(
            title,
            Style::default().fg(LABEL_GRAY).add_modifier(Modifier::BOLD),
        ),
        Span::styled(count, Style::default().fg(Color::DarkGray)),
        Span::styled(
            "─".repeat(width.saturating_sub(used)),
            Style::default().fg(SEP_GRAY),
        ),
    ])
}

fn finder_row(
    choice: &SessionFinderChoice,
    row: &PickerRow,
    state: Option<&SessionState>,
    columns: &FinderColumns,
    selected: bool,
    now: u64,
) -> Line<'static> {
    let mut styles = RowStyles::new(selected, Color::Gray);
    if !choice.titled && !selected {
        styles.label = Style::default().fg(DIM_TEXT).add_modifier(Modifier::ITALIC);
    }
    let bar = styles.bar;
    let pad = |n: usize| Span::styled(" ".repeat(n), bar);

    let mut spans = vec![styles.cursor(selected)];
    spans.push(match state {
        Some(state) => Span::styled(format!("{STATE_DOT} "), bar.fg(Dot::of(state).color(now))),
        None => pad(2),
    });

    let agent = if choice.agent_id == "claude" {
        String::new()
    } else {
        format!("[{}] ", choice.agent_id)
    };
    let label_budget = columns.label().saturating_sub(agent.chars().count());
    let label = first_line_truncated(&choice.label, label_budget);
    spans.push(Span::styled(agent.clone(), bar.fg(MUTED_TEXT)));
    spans.extend(highlight_spans(
        &label,
        &row.label_indices,
        styles.label,
        styles.label_hl,
    ));
    spans.push(pad(columns.label()
        - agent.chars().count().min(columns.label())
        - label.chars().count().min(label_budget)
        + COL_SEP));

    // `detail` is the project, a space, then the task title: split its
    // highlights back into the two columns.
    let offset = choice.task_offset();
    let project = first_line_truncated(&choice.project, columns.project);
    let project_hl: Vec<usize> = row
        .detail_indices
        .iter()
        .copied()
        .filter(|&i| i < offset - 1)
        .collect();
    spans.extend(highlight_spans(
        &project,
        &project_hl,
        if selected {
            styles.detail
        } else {
            Style::default().fg(LABEL_GRAY)
        },
        styles.detail_hl,
    ));
    spans.push(pad(
        columns.project.saturating_sub(project.chars().count()) + COL_SEP
    ));

    if let Some(task_w) = columns.task {
        match &choice.task {
            Some(task) => {
                let title =
                    first_line_truncated(task.title.lines().next().unwrap_or(""), task_w - 2);
                let task_hl: Vec<usize> = row
                    .detail_indices
                    .iter()
                    .filter(|&&i| i >= offset)
                    .map(|&i| i - offset)
                    .collect();
                let base = if selected {
                    styles.detail
                } else if task.stale {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(task_color(&task.task_id))
                };
                spans.push(Span::styled("󰢨 ", base));
                spans.extend(highlight_spans(&title, &task_hl, base, styles.detail_hl));
                spans.push(pad(
                    task_w - 2 - title.chars().count().min(task_w - 2) + COL_SEP
                ));
            }
            None => spans.push(pad(task_w + COL_SEP)),
        }
    }

    // Age is visual metadata only — the fuzzy filter never sees it.
    let age_secs = now.saturating_sub(choice.mtime_ms) / 1000;
    spans.push(Span::styled(
        format!(
            "{:>w$}",
            crate::models::relative_age_short(age_secs),
            w = FINDER_AGE_W
        ),
        if selected {
            styles.detail
        } else {
            Style::default().fg(Color::DarkGray)
        },
    ));
    Line::from(spans)
}

#[cfg(all(test, unix))]
mod task_link_picker_tests {
    use crate::app::{App, TaskLinkAction, TaskLinkChoice, TaskLinkPickerState, View};
    use crate::tasks::store::TaskStatus;
    use crate::test_util::with_temp_home;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    fn choice(label: &str, status: TaskStatus) -> TaskLinkChoice {
        TaskLinkChoice {
            label: label.into(),
            detail: status.board_label().to_string(),
            status: Some(status),
            action: TaskLinkAction::Link {
                task_id: format!("tk-{label}"),
                title: label.into(),
            },
        }
    }

    /// Style of the first buffer cell whose symbol is `ch`.
    fn cell_fg(buf: &ratatui::buffer::Buffer, ch: char) -> Option<Color> {
        let area = *buf.area();
        for y in 0..area.height {
            for x in 0..area.width {
                if buf[(x, y)].symbol() == ch.to_string() {
                    return buf[(x, y)].style().fg;
                }
            }
        }
        None
    }

    #[test]
    fn status_chip_uses_the_tasks_board_colors() {
        with_temp_home(|| {
            let mut app = App::new();
            app.task_link_picker = Some(TaskLinkPickerState::new(
                "sid".into(),
                "sid".into(),
                vec![
                    // Row 0 is selected — its chip drops the accent for
                    // readability on the white bar, so the color assertions
                    // target the unselected rows below it (Done's label
                    // shares no capitals with theirs).
                    choice("current", TaskStatus::Done),
                    choice("second", TaskStatus::Running),
                    choice("third", TaskStatus::Backlog),
                ],
                None,
            ));
            app.view = View::TaskLinkPicker;

            let backend = TestBackend::new(80, 20);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| super::render_task_link_picker(f, f.area(), &app))
                .expect("render");
            let buf = terminal.backend().buffer().clone();
            let rendered = buffer_to_string(&buf);

            assert!(rendered.contains("In Progress"), "chip text:\n{}", rendered);
            assert!(rendered.contains("To-Do"), "chip text:\n{}", rendered);
            // Board-label capitals are unique to the chips in this frame, so
            // their cells pin the chip color: In *P*rogress → the In Progress
            // column accent, *T*o-Do → the To-Do column accent.
            assert_eq!(cell_fg(&buf, 'P'), Some(Color::LightYellow));
            assert_eq!(cell_fg(&buf, 'T'), Some(crate::ui::palette::BACKLOG_BLUE));
        });
    }
}

#[cfg(all(test, unix))]
mod model_picker_tests {
    use crate::agent::{default_claude_models, AgentConfig, AgentKind, AgentModel};
    use crate::app::{App, ModelPickerState, View};
    use crate::test_util::with_temp_home;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn renders_filter_match_count_and_surviving_model() {
        with_temp_home(|| {
            let mut app = App::new();
            app.enter_model_picker();
            for c in "sn5".chars() {
                app.model_picker.as_mut().unwrap().push_filter(c);
            }
            assert_eq!(app.view, View::ModelPicker);

            let backend = TestBackend::new(70, 16);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| super::render_model_picker(f, f.area(), &app))
                .expect("render");
            let rendered = buffer_to_string(terminal.backend().buffer());

            assert!(rendered.contains("sn5▎"), "filter line:\n{}", rendered);
            assert!(rendered.contains("1/3"), "match count:\n{}", rendered);
            assert!(rendered.contains("Sonnet 5"), "matched row:\n{}", rendered);
            assert!(
                !rendered.contains("Opus 4.8") && !rendered.contains("Fable 5"),
                "filtered-out rows must not render:\n{}",
                rendered
            );
        });
    }

    #[test]
    fn renders_cycled_agent_and_its_models() {
        with_temp_home(|| {
            let mut app = App::new();
            app.model_picker = Some(ModelPickerState::new(
                "/tmp/proj".into(),
                "claude".into(),
                vec![
                    AgentConfig {
                        id: "claude".into(),
                        kind: AgentKind::Claude,
                        command: "claude".into(),
                        use_bridge: false,
                        models: default_claude_models(),
                    },
                    AgentConfig {
                        id: "pi-codex".into(),
                        kind: AgentKind::Pi,
                        command: "pi --provider openai-codex".into(),
                        use_bridge: true,
                        models: vec![AgentModel {
                            label: "GPT-5.6".into(),
                            id: "gpt-5.6".into(),
                        }],
                    },
                ],
            ));
            app.model_picker.as_mut().unwrap().cycle_agent();
            app.view = View::ModelPicker;

            let backend = TestBackend::new(80, 16);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| super::render_model_picker(f, f.area(), &app))
                .expect("render");
            let rendered = buffer_to_string(terminal.backend().buffer());

            assert!(rendered.contains("pi-codex"), "agent footer:\n{}", rendered);
            assert!(
                rendered.contains("GPT-5.6"),
                "configured model:\n{}",
                rendered
            );
            assert!(rendered.contains("gpt-5.6"), "model id:\n{}", rendered);
        });
    }
}

#[cfg(all(test, unix))]
mod session_finder_tests {
    use crate::agent::AgentKind;
    use crate::app::{App, SessionFinderChoice, SessionFinderState, View};
    use crate::models::{SessionState, TaskBadge};
    use crate::sessions::index::IndexedSession;
    use crate::test_util::with_temp_home;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn choice(sid: &str, title: Option<&str>, first: &str, hours_ago: u64) -> SessionFinderChoice {
        let session = IndexedSession {
            agent_id: "claude".into(),
            agent_kind: AgentKind::Claude,
            session_id: sid.into(),
            cwd: "/tmp/cc-hub".into(),
            project_name: "cc-hub".into(),
            jsonl_path: format!("/x/{sid}.jsonl").into(),
            mtime_ms: crate::ui::now_ms() - hours_ago * 3_600_000,
            title: title.map(str::to_string),
            first_message: Some(first.into()),
        };
        SessionFinderChoice::of(session, None)
    }

    fn render(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(110, 16)).expect("terminal");
        terminal
            .draw(|f| super::render_session_finder(f, f.area(), app))
            .expect("render");
        buffer_to_string(terminal.backend().buffer())
    }

    // Each row lines its project and task up in columns; a live session
    // carries its dot, and an untitled one falls back to its first message.
    #[test]
    fn rows_read_as_a_table_under_period_headings() {
        with_temp_home(|| {
            let mut linked = choice("a", Some("Polish"), "Hey", 0);
            linked.task = Some(TaskBadge {
                task_id: "tk-1".into(),
                title: "Hub polish".into(),
                priority: None,
                stale: false,
            });
            let mut finder = SessionFinderState::loading();
            finder.set_choices(vec![
                linked,
                choice("b", None, "why does the deep link open twice", 1),
                choice("c", Some("Push"), "Hey", 50),
            ]);
            let mut app = App::new();
            app.sessions.last_sessions = vec![crate::models::SessionInfo {
                session_id: "a".into(),
                state: SessionState::Processing,
                ..crate::test_util::session_info()
            }];
            app.session_finder = Some(finder);
            app.view = View::SessionFinder;

            let rendered = render(&app);
            assert!(rendered.contains("Today"), "heading:\n{rendered}");
            assert!(
                rendered.contains("why does the deep link"),
                "first message:\n{rendered}"
            );
            assert!(
                rendered.contains("󰢨 Hub polish"),
                "task column:\n{rendered}"
            );
            let column = |needle: &str| {
                rendered
                    .lines()
                    .filter(|l| l.contains(needle))
                    .map(|l| {
                        l[..l.find("cc-hub").expect("project column")]
                            .chars()
                            .count()
                    })
                    .next()
            };
            assert_eq!(
                column("Polish"),
                column("deep link"),
                "project column:\n{rendered}"
            );
        });
    }
}
