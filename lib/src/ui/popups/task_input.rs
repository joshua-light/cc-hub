//! The add/rename-task popup and its context box.

use super::widgets::{wrapped_rows, CURSOR};
use crate::app::{App, TaskField};
use crate::ui::common::{centered_fixed, popup_block};
use crate::ui::palette::{ACCENT_BLUE, DIM_TEXT};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::Frame;

/// The add/rename-task popup. Renaming is the one-line editor it always was;
/// adding shows a second field under it — the context box, where a pasted
/// spec, stack trace or ticket lands (see [`App::paste_into_input`]). Tab
/// moves between the two, enter commits both, esc cancels.
pub(crate) fn render_task_input(frame: &mut Frame, area: Rect, app: &App) {
    let renaming = app.tasks.renaming.is_some();
    let on_context = !renaming && app.tasks.field == TaskField::Context;

    let desired_w = 70u16.min(area.width);
    // Borders (2) plus the "  + " prefix (4) leave this much for the text, so
    // the height estimate matches what the wrapped Paragraph will occupy.
    let wrap_width = desired_w.saturating_sub(6) as usize;
    let mut input_line = app.tasks.input.clone();
    if !on_context {
        input_line.push(CURSOR);
    }
    let input_rows = wrapped_rows(&input_line, wrap_width);

    // Everything the popup holds besides the context body: borders, the
    // blank line above the task line, the task line, the blank + header
    // above the body, the blank + footer below it. The body takes the rows
    // the screen has left, so the popup grows with the text instead of
    // clipping it.
    let fixed_rows = 7u16.saturating_add(input_rows);
    let context_budget = area.height.saturating_sub(fixed_rows).max(1) as usize;
    let context_body = context_lines(&app.tasks.context, on_context, wrap_width, context_budget);

    let desired_h = if renaming {
        5u16.saturating_add(input_rows)
    } else {
        fixed_rows.saturating_add(context_body.len() as u16)
    }
    .max(9)
    .min(area.height);
    let popup = centered_fixed(area, desired_w, desired_h);
    frame.render_widget(Clear, popup);

    let (title, hint) = if renaming {
        (" Rename task ", " edits the text in place ")
    } else {
        (
            " New task ",
            " lands in To-Do · #tag !1–!4 inline · pasted text becomes context ",
        )
    };
    let block = popup_block(Span::styled(
        title,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        hint,
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    ));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let mut footer_spans = vec![
        Span::raw("  "),
        Span::styled(
            "[enter]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if renaming { " rename   " } else { " add   " },
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if !renaming {
        footer_spans.push(Span::styled(
            "[tab]",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
        footer_spans.push(Span::styled(
            if on_context {
                " task   "
            } else {
                " context   "
            },
            Style::default().fg(Color::DarkGray),
        ));
    }
    footer_spans.push(Span::styled(
        "[esc]",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));
    footer_spans.push(Span::styled(
        " cancel",
        Style::default().fg(Color::DarkGray),
    ));

    let mut lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled("  + ", Style::default().fg(Color::Green)),
            Span::styled(
                input_line,
                Style::default()
                    .fg(if on_context {
                        Color::Gray
                    } else {
                        Color::White
                    })
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];
    if !renaming {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!("  context ({} chars)", app.tasks.context.chars().count()),
            Style::default()
                .fg(if on_context { ACCENT_BLUE } else { DIM_TEXT })
                .add_modifier(Modifier::ITALIC),
        )));
        lines.extend(context_body);
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(footer_spans));

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// The context box's body, `budget` rows at most. Lines wrap by column, not
/// by word, so a pasted stack trace keeps its indentation and the row count
/// matches what the popup's Paragraph will draw. Text that does not fit is
/// summarised by one marker row: a focused box follows the cursor, so it
/// keeps the tail and counts the rows above; an unfocused one keeps the head
/// and counts the rows below. Empty context renders the hint that says what
/// the field is for.
fn context_lines(context: &str, focused: bool, width: usize, budget: usize) -> Vec<Line<'static>> {
    if context.is_empty() {
        return vec![Line::from(Span::styled(
            format!(
                "  {}paste or type anything the agent should know",
                if focused { CURSOR } else { ' ' }
            ),
            Style::default().fg(Color::DarkGray),
        ))];
    }
    let width = width.max(1);
    let mut rows: Vec<String> = context
        .lines()
        .flat_map(|raw| column_rows(raw, width))
        .collect();
    // A trailing newline means the next keystroke lands on a fresh row.
    if context.ends_with('\n') {
        rows.push(String::new());
    }
    if focused {
        match rows.last_mut() {
            Some(last) if last.chars().count() < width => last.push(CURSOR),
            _ => rows.push(CURSOR.to_string()),
        }
    }

    let budget = budget.max(1);
    let marker = if rows.len() > budget {
        let hidden = rows.len() - (budget - 1);
        if focused {
            rows.drain(..hidden);
            Some((0, format!("  … {} more rows above", hidden)))
        } else {
            rows.truncate(budget - 1);
            Some((rows.len(), format!("  … {} more rows", hidden)))
        }
    } else {
        None
    };

    let mut lines: Vec<Line<'static>> = rows
        .into_iter()
        .map(|row| {
            Line::from(Span::styled(
                format!("  {}", row),
                Style::default().fg(Color::Rgb(200, 200, 210)),
            ))
        })
        .collect();
    if let Some((at, text)) = marker {
        lines.insert(
            at,
            Line::from(Span::styled(text, Style::default().fg(Color::DarkGray))),
        );
    }
    lines
}

/// `raw` cut into rows of at most `width` chars; an empty line is one row.
fn column_rows(raw: &str, width: usize) -> Vec<String> {
    let mut chars = raw.chars().peekable();
    let mut rows = Vec::new();
    loop {
        rows.push(chars.by_ref().take(width).collect::<String>());
        if chars.peek().is_none() {
            return rows;
        }
    }
}

// Unix-only: `with_temp_home` isolates `$HOME` for `App::new()`'s loads,
// same as the todo-panel suite below.
#[cfg(all(test, unix))]
mod task_input_tests {
    use crate::app::{App, TaskField, View};
    use crate::test_util::with_temp_home;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn popup_grows_to_fit_long_input_instead_of_clipping() {
        with_temp_home(|| {
            let mut app = App::new();
            // Longer than the fixed 9-row popup could hold once wrapped (>4
            // rows at the ~64-column text width), so the popup must grow or
            // the footer hints get pushed out the bottom.
            let long = "investigate why the orchestrator retry backoff logic \
                keeps hammering the upstream scheduler after a transient \
                network partition and document every observed failure mode \
                plus the exact sequence of reconnect attempts so we can \
                finally write a deterministic regression test for it";
            app.tasks.input = long.to_string();
            app.view = View::TaskInput;

            let backend = TestBackend::new(90, 26);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| super::render_task_input(f, f.area(), &app))
                .expect("render");
            let rendered = buffer_to_string(terminal.backend().buffer());

            // The tail of the text would vanish first if the input clipped.
            for word in long.split_whitespace() {
                assert!(
                    rendered.contains(word),
                    "word {:?} should appear in the popup:\n{}",
                    word,
                    rendered
                );
            }
            // The footer sits below the input, so it's the first casualty of
            // a popup that didn't grow.
            for hint in ["[enter]", "add", "[esc]", "cancel"] {
                assert!(
                    rendered.contains(hint),
                    "footer hint {:?} should stay visible:\n{}",
                    hint,
                    rendered
                );
            }
        });
    }

    fn render(app: &App) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| super::render_task_input(f, f.area(), app))
            .expect("render");
        buffer_to_string(terminal.backend().buffer())
    }

    #[test]
    fn add_popup_shows_the_context_box_and_where_the_cursor_is() {
        with_temp_home(|| {
            let mut app = App::new();
            app.enter_task_input();
            app.tasks.input = "fix the parser".into();
            assert_eq!(app.view, View::TaskInput);

            // Empty context: the hint says what the field is for, and the
            // cursor is still on the task line.
            let empty = render(&app);
            assert!(empty.contains("fix the parser▎"), "task line:\n{}", empty);
            assert!(empty.contains("paste or type anything"), "hint:\n{}", empty);

            // A multi-line paste fills the box and takes the cursor with it.
            app.paste_into_input("first line\nsecond line");
            assert_eq!(app.tasks.field, TaskField::Context);
            let pasted = render(&app);
            assert!(pasted.contains("first line"), "context:\n{}", pasted);
            assert!(pasted.contains("second line▎"), "cursor:\n{}", pasted);
            assert!(
                !pasted.contains("fix the parser▎"),
                "cursor moved:\n{}",
                pasted
            );
        });
    }

    #[test]
    fn a_long_paste_follows_the_cursor_and_counts_the_rest() {
        with_temp_home(|| {
            let mut app = App::new();
            app.enter_task_input();
            app.tasks.input = "big one".into();
            // More rows than a 24-row terminal can give the box.
            let paste: String = (1..=40).map(|n| format!("line {}\n", n)).collect();
            app.paste_into_input(&paste);
            assert_eq!(app.tasks.field, TaskField::Context);

            // Focused: the cursor is at the end, so the tail is what shows.
            let focused = render(&app);
            assert!(focused.contains("line 40"), "tail:\n{}", focused);
            assert!(!focused.contains("line 1 "), "head hidden:\n{}", focused);
            assert!(
                focused.contains("more rows above"),
                "overflow marker:\n{}",
                focused
            );
            assert!(focused.contains("[esc]"), "footer stays:\n{}", focused);

            // Back on the task line the box is a preview, so the head shows.
            app.tasks.field = TaskField::Text;
            let unfocused = render(&app);
            assert!(unfocused.contains("line 1 "), "head:\n{}", unfocused);
            assert!(
                !unfocused.contains("line 40"),
                "tail hidden:\n{}",
                unfocused
            );
            assert!(
                unfocused.contains("more rows") && !unfocused.contains("above"),
                "overflow marker:\n{}",
                unfocused
            );
        });
    }

    #[test]
    fn typed_context_wraps_and_the_popup_grows_to_keep_the_cursor() {
        with_temp_home(|| {
            let mut app = App::new();
            app.enter_task_input();
            app.tasks.input = "typed".into();
            app.tasks.field = TaskField::Context;
            // One long line, as typing produces (enter submits, it does not
            // break lines): far wider than the box, so it has to wrap.
            app.tasks.context = "0123456789".repeat(20) + "END";

            let rendered = render(&app);
            assert!(
                rendered.contains("END▎"),
                "cursor on the tail:\n{}",
                rendered
            );
            assert!(
                !rendered.contains("more rows"),
                "fits once wrapped, nothing hidden:\n{}",
                rendered
            );
            assert!(rendered.contains("[esc]"), "footer stays:\n{}", rendered);
        });
    }

    #[test]
    fn rename_popup_has_no_context_field() {
        with_temp_home(|| {
            let mut app = App::new();
            let id = app.tasks.board.add("plain").unwrap().unwrap();
            app.focus_task(&id);
            assert!(app.enter_task_rename());

            let rendered = render(&app);
            assert!(rendered.contains("plain▎"), "task line:\n{}", rendered);
            assert!(
                !rendered.contains("context"),
                "no context box:\n{}",
                rendered
            );
        });
    }
}
