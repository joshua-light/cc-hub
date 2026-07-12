//! The project chip strip at the top of the Projects tab.

use crate::app::App;
use crate::ui::palette::{DIM_TEXT, LABEL_GRAY};
use crate::ui::BAND_BG;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// Horizontal strip of project "chips". Selected chip is bold/inverse with
/// per-column counts (P·R·Rv·M·D·F). Cycled with `[` / `]`.
/// A trailing amber 󰒲N is shown only when backlog > 0.
pub(crate) fn render_project_chip_strip(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }
    // Background band so the strip reads as a header even on dark themes.
    let band = Style::default().bg(BAND_BG);
    frame.render_widget(Paragraph::new("").style(band), area);

    let chip_row = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    let path_row = if area.height >= 2 {
        Some(Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: 1,
        })
    } else {
        None
    };

    let snap = &app.projects.snapshot;
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(snap.projects.len() * 4 + 2);
    spans.push(Span::styled(
        "  󰉋 ",
        Style::default().fg(LABEL_GRAY).bg(BAND_BG),
    ));
    spans.push(Span::styled(
        " P·R·Rv·M·D  ",
        Style::default().fg(DIM_TEXT).bg(BAND_BG),
    ));
    let mut selected_start: usize = 0;
    let mut selected_end: usize = 0;
    for (idx, p) in snap.projects.iter().enumerate() {
        let tasks = snap.tasks.get(&p.id);
        let mut planning = 0usize;
        let mut running = 0usize;
        let mut review = 0usize;
        let mut merging = 0usize;
        let mut done = 0usize;
        let mut backlog = 0usize;
        if let Some(v) = tasks {
            for t in v {
                match t.status {
                    crate::orchestrator::TaskStatus::Running => {
                        if t.workers.is_empty() {
                            planning += 1;
                        } else {
                            running += 1;
                        }
                    }
                    // Personal-board state; never present in a project scan,
                    // but bucket it with the workerless-Running chip anyway.
                    crate::orchestrator::TaskStatus::Planning => planning += 1,
                    crate::orchestrator::TaskStatus::Review => review += 1,
                    crate::orchestrator::TaskStatus::Merging => merging += 1,
                    crate::orchestrator::TaskStatus::Done => done += 1,
                    crate::orchestrator::TaskStatus::Backlog => backlog += 1,
                }
            }
        }
        let selected = idx == app.projects.sel;
        let label = format!(" {} ", p.name);
        // Compact P·R·Rv·M·D counts. Review/Merging squeezed to two-letter
        // labels in the column headers; here they're positional.
        let counts = format!(" {}·{}·{}·{}·{} ", planning, running, review, merging, done);
        let (chip_fg, chip_bg) = if selected {
            (Color::Black, Color::Rgb(190, 200, 230))
        } else if planning + running + merging > 0 {
            (Color::Rgb(220, 220, 235), Color::Rgb(40, 50, 70))
        } else {
            (Color::Rgb(150, 150, 165), Color::Rgb(30, 30, 40))
        };
        let counts_bg = if selected {
            Color::Rgb(140, 150, 180)
        } else {
            Color::Rgb(20, 25, 35)
        };
        let counts_fg = if selected {
            Color::Black
        } else if planning + running + review + merging > 0 {
            Color::Rgb(160, 220, 180)
        } else {
            Color::Rgb(120, 120, 140)
        };
        if selected {
            selected_start = spans.iter().map(|s| s.content.chars().count()).sum();
        }
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(chip_fg)
                .bg(chip_bg)
                .add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ));
        spans.push(Span::styled(
            counts,
            Style::default().fg(counts_fg).bg(counts_bg),
        ));
        if backlog > 0 {
            let (bl_fg, bl_bg) = if selected {
                (Color::Black, Color::Rgb(200, 160, 80))
            } else {
                (Color::Rgb(220, 175, 95), Color::Rgb(50, 40, 22))
            };
            spans.push(Span::styled(
                format!(" 󰒲 {} ", backlog),
                Style::default().fg(bl_fg).bg(bl_bg),
            ));
        }
        spans.push(Span::styled(" ", band));
        if selected {
            selected_end = spans.iter().map(|s| s.content.chars().count()).sum();
        }
    }
    let visible_w = chip_row.width as usize;
    let chip_scroll = if selected_end > visible_w {
        selected_start.saturating_sub(4).min(u16::MAX as usize) as u16
    } else {
        0
    };
    frame.render_widget(
        Paragraph::new(Line::from(spans))
            .style(band)
            .scroll((0, chip_scroll)),
        chip_row,
    );

    if let (Some(row), Some(p)) = (path_row, app.selected_project()) {
        let root = p.root.display().to_string();
        // The 5-cell prefix ("    …" when truncated, 5 spaces otherwise) keeps
        // the path aligned in the same column either way.
        let max = (row.width as usize).saturating_sub(5);
        let root = if root.chars().count() > max {
            let cut = root.chars().count().saturating_sub(max);
            format!("    …{}", root.chars().skip(cut).collect::<String>())
        } else {
            format!("     {}", root)
        };
        let line = Line::from(Span::styled(
            root,
            Style::default().fg(DIM_TEXT).bg(BAND_BG),
        ));
        frame.render_widget(Paragraph::new(line).style(band), row);
    }
}
