//! The new-session / assign-task folder picker (browse, bookmarks, places)
//! and the `gh repo create` input drawn on top of it.

use super::widgets::{highlight_spans, render_filter_row, RowStyles};
use crate::app::App;
use crate::folder_picker::{FolderPicker, PickerMode, PlaceSource};
use crate::ui::common::{centered_fixed, popup_block, CURSOR};
use crate::ui::palette::{ACCENT_BLUE, DIM_TEXT};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::Frame;

pub(crate) fn render_folder_picker(frame: &mut Frame, area: Rect, app: &App) {
    let Some(picker) = app.folder_picker.as_ref() else {
        return;
    };
    let assigning = app.tasks.pending_assign.is_some();
    if picker.mode == PickerMode::Places {
        render_places_picker(frame, area, picker, assigning);
        return;
    }
    let bookmarks_mode = picker.mode == PickerMode::Bookmarks;

    let popup = centered_fixed(area, 80, 24);
    frame.render_widget(Clear, popup);

    let (title_text, footer_text, empty_text) = if bookmarks_mode {
        (
            " New session · bookmarks ",
            " j/k:move · enter/space:pick · m:unbookmark · esc:cancel ",
            "  (no bookmarks — press N to browse, then m on a folder)",
        )
    } else if assigning {
        (
            " Assign task · pick folder ",
            " enter:descend · bksp:parent · space/.:pick · tab:places · esc:cancel ",
            "  (no subdirectories)",
        )
    } else {
        (
            " New session · pick folder ",
            " enter:descend · bksp:parent · space:pick · .:pick cwd · m:bookmark · c/C:gh new · esc:cancel ",
            "  (no subdirectories)",
        )
    };

    let block = popup_block(Span::styled(
        title_text,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(footer_text, Style::default().fg(DIM_TEXT)));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height < 3 {
        return;
    }

    let path_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let header_line = if bookmarks_mode {
        Line::from(vec![
            Span::styled(" ★ ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("{} bookmark(s)", picker.entries.len()),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled(" 󰉋 ", Style::default().fg(Color::Cyan)),
            Span::styled(
                picker.current_dir.display().to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])
    };
    frame.render_widget(Paragraph::new(header_line), path_area);

    let list_h = inner.height - 2;
    let list_area = Rect::new(inner.x, inner.y + 2, inner.width, list_h);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if picker.entries.is_empty() {
        lines.push(Line::from(Span::styled(
            empty_text,
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let visible = list_h as usize;
        let start = picker.selection.saturating_sub(visible.saturating_sub(1));
        for (i, name) in picker.entries.iter().enumerate().skip(start).take(visible) {
            let selected = i == picker.selection;
            let (cursor_marker, style) = if selected {
                (
                    "▶ ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("  ", Style::default().fg(Color::Rgb(200, 200, 210)))
            };
            let display = if bookmarks_mode {
                name.clone()
            } else {
                format!("{}/", name)
            };
            // In browse mode, mark already-bookmarked subdirs with a star
            // so the user doesn't re-bookmark by accident.
            let star_span =
                if !bookmarks_mode && app.bookmarks.contains(&picker.current_dir.join(name)) {
                    Span::styled(
                        "★ ",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::raw("")
                };
            lines.push(Line::from(vec![
                Span::styled(cursor_marker, style),
                star_span,
                Span::styled(display, style),
            ]));
        }
    }

    frame.render_widget(Paragraph::new(lines), list_area);
}

/// Places mode of the assign / new-session picker: a flat, fuzzy-filterable
/// list of known directories (bookmarks, recent cwds).
/// The top row is the live filter; matched chars are highlighted in each row.
fn render_places_picker(frame: &mut Frame, area: Rect, picker: &FolderPicker, assigning: bool) {
    let popup = centered_fixed(area, 80, 24);
    frame.render_widget(Clear, popup);

    let title = if assigning {
        " Assign task · pick project "
    } else {
        " New session · pick project "
    };
    let block = popup_block(Span::styled(
        title,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        " type:filter · ↑/↓:move · enter/space:pick · tab:browse · esc:cancel ",
        Style::default().fg(DIM_TEXT),
    ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height < 3 {
        return;
    }

    render_filter_row(
        frame,
        inner,
        &picker.filter,
        format!("{}/{} ", picker.rows.len(), picker.places.len()),
    );

    let list_h = inner.height - 2;
    let list_area = Rect::new(inner.x, inner.y + 2, inner.width, list_h);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if picker.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no matches — backspace to widen, tab to browse)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let visible = list_h as usize;
        let start = picker.selection.saturating_sub(visible.saturating_sub(1));
        for (i, row) in picker.rows.iter().enumerate().skip(start).take(visible) {
            let Some(place) = picker.places.get(row.place) else {
                continue;
            };
            let selected = i == picker.selection;
            let styles = RowStyles::new(selected, Color::Rgb(200, 200, 210));
            let bar = styles.bar;
            let cursor = styles.cursor(selected);
            let badge = match place.source {
                PlaceSource::Bookmark => Span::styled("★ ", bar.fg(Color::Yellow)),
                PlaceSource::Recent => Span::styled("· ", bar.fg(Color::DarkGray)),
            };
            let mut spans = vec![cursor, badge];
            spans.extend(highlight_spans(
                &place.name,
                &row.name_indices,
                styles.label,
                styles.label_hl,
            ));
            spans.push(Span::styled("  ", bar));
            spans.extend(highlight_spans(
                &place.display_path,
                &row.path_indices,
                styles.detail,
                styles.detail_hl,
            ));
            lines.push(Line::from(spans));
        }
    }
    frame.render_widget(Paragraph::new(lines), list_area);
}

pub(crate) fn render_gh_create_input(frame: &mut Frame, area: Rect, app: &App) {
    let Some(input) = app.gh_create_input.as_ref() else {
        return;
    };

    let popup = centered_fixed(area, 70, 9);
    frame.render_widget(Clear, popup);

    let (vis_label, vis_color) = if input.private {
        ("private", Color::Rgb(220, 170, 90))
    } else {
        ("public", Color::Rgb(120, 200, 140))
    };

    let block = popup_block(Span::styled(
        " gh repo create ",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        " type name · tab: toggle public/private · enter: create · esc: cancel ",
        Style::default().fg(DIM_TEXT),
    ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height < 4 || inner.width == 0 {
        return;
    }

    let cwd_line = Line::from(vec![
        Span::styled(" in ", Style::default().fg(Color::DarkGray)),
        Span::styled(input.cwd.clone(), Style::default().fg(ACCENT_BLUE)),
    ]);

    let mut name_str = input.name.clone();
    name_str.push(CURSOR);
    let name_line = Line::from(vec![
        Span::styled(" name: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            name_str,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    let vis_line = Line::from(vec![
        Span::styled(" visibility: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            vis_label,
            Style::default().fg(vis_color).add_modifier(Modifier::BOLD),
        ),
    ]);

    let lines = vec![cwd_line, Line::raw(""), name_line, vis_line];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

// Unix-only: `with_temp_home` isolates `$HOME` for `App::new()`'s loads.
#[cfg(all(test, unix))]
mod places_picker_tests {
    use crate::app::{App, View};
    use crate::folder_picker::{FolderPicker, Place, PlaceSource};
    use crate::test_util::with_temp_home;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    #[test]
    fn renders_filter_match_count_and_surviving_rows() {
        with_temp_home(|| {
            let mut app = App::new();
            let mut picker = FolderPicker::new_places(vec![
                Place::new(PathBuf::from("/g/self/cc-hub"), PlaceSource::Bookmark),
                Place::new(PathBuf::from("/g/self/reddit"), PlaceSource::Recent),
            ]);
            for c in "hub".chars() {
                picker.push_filter(c);
            }
            app.folder_picker = Some(picker);
            app.view = View::FolderPicker;

            let backend = TestBackend::new(90, 26);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| super::render_folder_picker(f, f.area(), &app))
                .expect("render");
            let rendered = buffer_to_string(terminal.backend().buffer());

            assert!(rendered.contains("cc-hub"), "name row:\n{}", rendered);
            assert!(rendered.contains("/g/self/cc-hub"), "path:\n{}", rendered);
            assert!(rendered.contains("hub▎"), "filter line:\n{}", rendered);
            assert!(rendered.contains("1/2"), "match count:\n{}", rendered);
            assert!(
                !rendered.contains("reddit"),
                "filtered-out row must not render:\n{}",
                rendered
            );
        });
    }
}
