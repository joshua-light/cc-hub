//! Projects tab: the chip strip, the kanban board with active/collapsed task
//! cards (PR badges, merge progress, agent dots, ctx bars), the backlog popup,
//! and the progressive-disclosure result popup with its artifact card renderers.
//!
//! The tab is split into focused submodules; this module owns the top-level
//! [`render_projects_body`] dispatcher and re-exports every renderer/helper so
//! the existing `ui::projects::X` paths keep resolving.

mod agents;
mod artifacts;
mod backlog;
mod board;
mod cards;
mod chips;
mod diff;
mod result_popup;
mod task_cards;

pub(crate) use agents::*;
pub(crate) use artifacts::*;
pub(crate) use backlog::*;
pub(crate) use board::*;
pub(crate) use cards::*;
pub(crate) use chips::*;
pub(crate) use diff::*;
pub(crate) use result_popup::*;
pub(crate) use task_cards::*;

use crate::app::App;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

pub(crate) fn render_projects_body(frame: &mut Frame, area: Rect, app: &App) {
    let snap = &app.projects.snapshot;

    if snap.projects.is_empty() {
        let empty = Paragraph::new(Line::from(vec![
            Span::styled(
                "No projects registered yet. ",
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                "Press N to register a folder, then n to start a task.",
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL).title(" Projects "));
        frame.render_widget(empty, area);
        return;
    }

    // Top: project chip strip (1 line) + spacer (1 line). The PR-flow
    // design serialises merges through the project-level merge lock, so
    // file-level contention strips no longer apply — at most one task is
    // ever in the Merging column.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(area);
    render_project_chip_strip(frame, rows[0], app);
    render_kanban_board(frame, rows[1], app);
}
