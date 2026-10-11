//! The Agents tab body: the agents down the side, each its status and how
//! its last run went, and the selected one's page beside them (see
//! `page`).

use super::page::render_page;
use super::{mark, off};
use crate::app::{App, View};
use crate::harness::AgentSnapshot;
use crate::ui::common::age;
use crate::ui::palette::{DIM_TEXT, LABEL_GRAY};
use crate::ui::split::{keep_row_in_view, render_row, SidebarRow, Split};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

pub(crate) fn render_agents_body(frame: &mut Frame, area: Rect, app: &mut App) {
    render_agents_body_at(frame, area, app, crate::harness::now_unix());
}

fn render_agents_body_at(frame: &mut Frame, area: Rect, app: &mut App, now: i64) {
    app.render.split = Default::default();
    if area.height < 1 || area.width < 10 {
        return;
    }
    if app.harness.agents.is_empty() {
        let msg = if !app.harness.loaded {
            " Reading agents …".to_string()
        } else {
            let root = crate::harness::root()
                .map(|r| r.display().to_string())
                .unwrap_or_else(|| "~/.cc-hub/agents".into());
            format!(
                " No agents yet. Scaffold one with `cc-hub agent new <name>` — specs live in {}",
                root
            )
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(msg, Style::default().fg(DIM_TEXT))))
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }

    let split = Split::of(area, app.view == View::AgentDetail);
    app.render.split.page = split.page;
    render_list(frame, split.sidebar, app, now);
    if !split.page.is_empty() {
        render_page(frame, split, app, now);
    }
    split.dim_unfocused(frame);
}

fn render_list(frame: &mut Frame, area: Rect, app: &mut App, now: i64) {
    app.render.split.sidebar = area;
    let scroll = &mut app.render.agents_scroll;
    keep_row_in_view(scroll, app.harness.selected, area.height);
    let first = *scroll as usize;
    let shown = app
        .harness
        .agents
        .iter()
        .enumerate()
        .skip(first)
        .take(area.height as usize);
    for (i, agent) in shown {
        let y = area.y + (i - first) as u16;
        let row = sidebar_row(agent, now);
        render_row(
            frame,
            Rect::new(area.x, y, area.width, 1),
            row,
            i == app.harness.selected,
        );
    }
    let end = app.harness.agents.len().min(first + area.height as usize);
    app.render.split.rows = (first..end).map(|i| Some((0, i))).collect();
}

/// The mark, the name in white as a session's is — gray while the agent is
/// off — and the last run at the far end.
fn sidebar_row(agent: &AgentSnapshot, now: i64) -> SidebarRow {
    let name = match off(agent) {
        true => LABEL_GRAY,
        false => Color::White,
    };
    SidebarRow {
        mark: mark(agent, crate::ui::now_ms()),
        name: agent.name.clone(),
        name_style: Style::default().fg(name),
        aside: last_run(agent, now),
    }
}

/// `▸ 40s` while a run is under way, else `✓ 12m` or `✗ 3d` for the last.
fn last_run(agent: &AgentSnapshot, now: i64) -> Span<'static> {
    if let Some(t) = &agent.state.ticking {
        return Span::styled(
            format!("▸ {}", age(now, t.since)),
            Style::default().fg(Color::Green),
        );
    }
    match agent.last_run() {
        Some(rec) if rec.ok => Span::styled(
            format!("✓ {}", age(now, rec.at)),
            Style::default().fg(DIM_TEXT),
        ),
        Some(rec) => Span::styled(
            format!("✗ {}", age(now, rec.at)),
            Style::default().fg(Color::Red),
        ),
        None => Span::raw(""),
    }
}

// `App::new()` loads from `$HOME`, which only unix tests can redirect.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::ui::agents::fixtures::{snap, ticked, NOW};
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(agents: Vec<AgentSnapshot>, view: View, w: u16) -> (String, App) {
        let mut out = String::new();
        let mut kept = None;
        crate::test_util::with_temp_home(|| {
            let mut app = App::new();
            app.harness.update(agents);
            app.view = view;
            let mut terminal = Terminal::new(TestBackend::new(w, 24)).expect("terminal");
            terminal
                .draw(|f| render_agents_body_at(f, f.area(), &mut app, NOW))
                .expect("render");
            out = buffer_to_string(terminal.backend().buffer());
            kept = Some(app);
        });
        (out, kept.expect("rendered"))
    }

    #[test]
    fn the_list_names_each_agent_and_its_last_run() {
        let (out, app) = render(
            vec![snap("bb-prs", ticked(true)), snap("jira", ticked(false))],
            View::Grid,
            120,
        );
        let rows: Vec<&str> = out.lines().collect();
        let sidebar = |y: usize| rows[y].chars().take(32).collect::<String>();
        assert!(
            sidebar(0).contains("● bb-prs") && sidebar(0).contains("✓ 5m"),
            "{out}"
        );
        assert!(
            sidebar(1).contains("● jira") && sidebar(1).contains("✗ 5m"),
            "{out}"
        );
        assert_eq!(app.render.split.rows, vec![Some((0, 0)), Some((0, 1))]);
    }

    // The selected agent's page sits beside the list, whichever side has
    // the keyboard.
    #[test]
    fn the_page_beside_the_list_is_the_selected_agent() {
        let (out, _) = render(vec![snap("bb-prs", ticked(false))], View::Grid, 120);
        assert!(out.contains("Last run failed 5m ago"), "{out}");
        assert!(out.contains("poll 5m"), "{out}");
    }

    #[test]
    fn narrow_screens_do_not_panic() {
        for w in [12, 40, 60] {
            let _ = render(vec![snap("x", ticked(true))], View::Grid, w);
            let _ = render(vec![snap("x", ticked(true))], View::AgentDetail, w);
        }
    }
}
