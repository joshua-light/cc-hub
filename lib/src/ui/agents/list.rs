//! The Agents tab body: the agents down the side, each its mark and name,
//! and the selected one's page beside them (see `page`).

use super::mark;
use super::page::render_page;
use crate::app::{App, View};
use crate::harness::AgentSnapshot;
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
                .unwrap_or_else(|| "~/.hal/agents".into());
            format!(
                " No agents yet. Scaffold one with `hal agent new <name>` — specs live in {}",
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
    render_list(frame, split, app);
    if !split.page.is_empty() {
        render_page(frame, split, app, now);
    }
    split.dim_unfocused(frame);
}

fn render_list(frame: &mut Frame, split: Split, app: &mut App) {
    let area = split.sidebar;
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
        let row = sidebar_row(agent);
        render_row(
            frame,
            Rect::new(area.x, y, area.width, 1),
            row,
            (i == app.harness.selected).then(|| split.cursor_bg()),
        );
    }
    let end = app.harness.agents.len().min(first + area.height as usize);
    app.render.split.rows = (first..end).map(|i| Some((0, i))).collect();
}

/// The mark and the name, white as a session's is — gray while the agent
/// is off. How its runs went is on its page.
fn sidebar_row(agent: &AgentSnapshot) -> SidebarRow {
    let name = match agent.is_off() {
        true => LABEL_GRAY,
        false => Color::White,
    };
    SidebarRow {
        mark: mark(agent, crate::ui::now_ms()),
        name: agent.name.clone(),
        name_style: Style::default().fg(name),
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

    // A row is the mark and the name, nothing after it; the agents that run
    // come first, the paused ones after them.
    #[test]
    fn the_list_names_each_agent_running_ones_first() {
        let paused = crate::harness::AgentState {
            paused: true,
            ..ticked(true)
        };
        let (out, app) = render(
            vec![
                snap("aa-paused", paused),
                snap("bb-prs", ticked(true)),
                snap("jira", ticked(false)),
            ],
            View::Grid,
            120,
        );
        let rows: Vec<&str> = out.lines().collect();
        let sidebar = |y: usize| rows[y].chars().take(32).collect::<String>();
        assert_eq!(sidebar(0).trim_end(), " ● bb-prs", "{out}");
        assert_eq!(sidebar(1).trim_end(), " ● jira", "{out}");
        assert!(sidebar(2).contains("aa-paused"), "{out}");
        assert_eq!(
            app.render.split.rows,
            vec![Some((0, 0)), Some((0, 1)), Some((0, 2))]
        );
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
