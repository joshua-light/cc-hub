//! The Backlog popup: split list/body view of a project's queued tasks.

use crate::app::App;
use crate::models;
use crate::ui::common::{centered_fixed, popup_block};
use crate::ui::now_ms;
use crate::ui::palette::{BACKLOG_BLUE, DIM_TEXT, FAINT_TEXT, GRAY_80};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::TASK_META_DIM;

pub(crate) fn render_backlog(frame: &mut Frame, area: Rect, app: &App) {
    let popup = centered_fixed(area, 120, 30);
    frame.render_widget(Clear, popup);

    let tasks = app.backlog_tasks();
    let project_name = app
        .selected_project()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "no project".to_string());
    let title_text = format!(" Backlog · {} ", project_name);
    let selected_label = if tasks.is_empty() {
        "".to_string()
    } else {
        format!(
            " · {}/{} ",
            app.projects.backlog_sel.min(tasks.len() - 1) + 1,
            tasks.len()
        )
    };
    let block = popup_block(Span::styled(
        title_text,
        Style::default()
            .fg(BACKLOG_BLUE)
            .add_modifier(Modifier::BOLD),
    ))
    .title_bottom(Span::styled(
        format!(
            " j/k navigate · s/enter start · x delete · esc/q close{}",
            selected_label
        ),
        Style::default().fg(Color::DarkGray),
    ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if tasks.is_empty() {
        let empty = Paragraph::new(vec![
            Line::from(Span::styled(
                "No backlog tasks for this project.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::raw(""),
            Line::from(Span::styled(
                "Queue one with: cc-hub task create --backlog --prompt \"…\"",
                Style::default().fg(GRAY_80),
            )),
        ])
        .alignment(Alignment::Center);
        frame.render_widget(empty, inner);
        return;
    }

    let (list_area, body_area) = if inner.width >= 60 {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(40), Constraint::Min(0)])
            .split(inner);
        (chunks[0], Some(chunks[1]))
    } else {
        (inner, None)
    };

    let max_w = list_area.width.saturating_sub(4) as usize;
    let rows_per_task = 3usize;
    let visible_tasks = ((list_area.height as usize) / rows_per_task).max(1);
    let sel = app.projects.backlog_sel.min(tasks.len() - 1);
    let scroll_top = if tasks.len() <= visible_tasks || sel < visible_tasks {
        0
    } else {
        sel + 1 - visible_tasks
    };
    let mut lines: Vec<Line> = Vec::with_capacity(visible_tasks * rows_per_task);
    let now_secs = now_ms() / 1000;
    for (i, t) in tasks
        .iter()
        .enumerate()
        .skip(scroll_top)
        .take(visible_tasks)
    {
        let selected = i == sel;
        let arrow = if selected { "▌ " } else { "  " };
        let has_title = t.title.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
        let id_short = crate::orchestrator::short_task_id(&t.task_id);
        let title_style = if selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let age_secs = now_secs.saturating_sub(t.created_at as u64);
        let age = format!("{:>4}", models::relative_age_short(age_secs));
        if has_title {
            let title_text = t.title.as_deref().unwrap().to_string();
            lines.push(Line::from(vec![
                Span::styled(arrow, Style::default().fg(BACKLOG_BLUE)),
                Span::styled(title_text, title_style),
            ]));
            let preview = models::first_line_truncated(
                &t.prompt,
                max_w.saturating_sub(id_short.len() + age.len() + 8),
            );
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(id_short, Style::default().fg(Color::DarkGray)),
                Span::styled("  ", Style::default()),
                Span::styled(age, Style::default().fg(TASK_META_DIM)),
                Span::styled("  ", Style::default()),
                Span::styled(preview, Style::default().fg(DIM_TEXT)),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled(arrow, Style::default().fg(BACKLOG_BLUE)),
                Span::styled(format!("#{}", id_short), title_style),
                Span::styled(" · pending title", Style::default().fg(Color::DarkGray)),
            ]));
            let preview =
                models::first_line_truncated(&t.prompt, max_w.saturating_sub(age.len() + 6));
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(age, Style::default().fg(TASK_META_DIM)),
                Span::styled("  ", Style::default()),
                Span::styled(preview, Style::default().fg(DIM_TEXT)),
            ]));
        }
        lines.push(Line::from(""));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), list_area);

    if let (Some(body_area), Some(task)) = (body_area, tasks.get(sel)) {
        let separator = Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(Color::Rgb(60, 60, 80)));
        let body_inner = separator.inner(body_area);
        frame.render_widget(separator, body_area);

        let mut body_lines: Vec<Line> = Vec::new();
        if let Some(title) = task.title.as_deref().filter(|s| !s.is_empty()) {
            body_lines.push(Line::from(Span::styled(
                title,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        body_lines.push(Line::from(Span::styled(
            crate::orchestrator::short_task_id(&task.task_id),
            Style::default().fg(Color::DarkGray),
        )));
        body_lines.push(Line::raw(""));
        for line in task.prompt.lines() {
            body_lines.push(Line::from(Span::styled(
                line,
                Style::default().fg(FAINT_TEXT),
            )));
        }
        frame.render_widget(
            Paragraph::new(body_lines).wrap(Wrap { trim: false }),
            body_inner,
        );
    }
}

#[cfg(test)]
mod backlog_popup_tests {
    use crate::app::App;
    use crate::orchestrator::{Project, TaskState};
    use crate::projects_scan::ProjectsSnapshot;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn render_popup(prompts: &[&str], titles: &[Option<&str>]) -> String {
        assert_eq!(prompts.len(), titles.len());
        let now = crate::orchestrator::now_unix_secs();
        let project = Project {
            id: "p-backlog".into(),
            name: "backlog".into(),
            root: PathBuf::from("/tmp/backlog"),
            created_at: now,
            build_cmd: None,
        };
        let tasks: Vec<Arc<TaskState>> = prompts
            .iter()
            .zip(titles.iter())
            .map(|(p, title)| {
                let mut t =
                    TaskState::new_backlog(project.id.clone(), project.root.clone(), (*p).into());
                t.title = title.map(|s| s.to_string());
                Arc::new(t)
            })
            .collect();
        let mut tasks_map = HashMap::new();
        tasks_map.insert(project.id.clone(), tasks);
        let snap = ProjectsSnapshot {
            projects: vec![project],
            tasks: tasks_map,
            titling: HashSet::new(),
            merge_lock_holders: HashMap::new(),
            merge_lock_holder_pr_ids: HashMap::new(),
            pr_summaries: HashMap::new(),
        };
        let mut app = App::new();
        app.update_projects(snap);
        app.open_backlog();
        let backend = TestBackend::new(100, 22);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| super::render_backlog(f, f.area(), &app))
            .expect("render");
        buffer_to_string(terminal.backend().buffer())
    }

    /// Renders mixed titled + untitled tasks and dumps the buffer to the path
    /// in `CC_HUB_BACKLOG_RENDER_DUMP`. Used to capture a proof-of-fix artifact
    /// for PRs; skipped under normal `cargo test`.
    #[test]
    #[ignore]
    fn dump_render_for_artifact() {
        let Some(dest) = std::env::var_os("CC_HUB_BACKLOG_RENDER_DUMP") else {
            return;
        };
        let prompts = [
            "wire up exporter for daily reports",
            "refactor the merge-lock retry policy",
            "investigate flaky CI on macOS runners",
            "ship the new exporter",
        ];
        let titles: [Option<&str>; 4] = [None, None, None, Some("Exporter rollout")];
        let plain = render_popup(&prompts, &titles);
        std::fs::write(&dest, plain).expect("write dump");
    }

    /// Like `dump_render_for_artifact`, but the focused task carries a
    /// realistic multi-line prompt (bug repro / fix sketch / acceptance
    /// criteria) — the case the split-pane refactor was motivated by.
    /// Dump path comes from `CC_HUB_BACKLOG_RENDER_DUMP_MULTILINE`.
    #[test]
    #[ignore]
    fn dump_render_for_artifact_multiline() {
        let Some(dest) = std::env::var_os("CC_HUB_BACKLOG_RENDER_DUMP_MULTILINE") else {
            return;
        };
        let multiline = "\
In lib/src/ui.rs::render_backlog, each backlog entry only shows a one-line\n\
preview. Explorer-loop tasks carry full repro + fix sketch + acceptance\n\
criteria — none of which is visible.\n\
\n\
Fix: split the popup into list (left) + body (right). Reuse j/k.\n\
\n\
Acceptance:\n\
- Full prompt visible without leaving the popup.\n\
- Narrow terminals clip gracefully.\n\
- Existing keybinds keep working.";
        let prompts = [
            multiline,
            "refactor the merge-lock retry policy",
            "investigate flaky CI on macOS runners",
        ];
        let titles: [Option<&str>; 3] = [Some("Backlog popup: show full prompt"), None, None];
        let plain = render_popup(&prompts, &titles);
        std::fs::write(&dest, plain).expect("write dump");
    }

    #[test]
    fn untitled_entries_do_not_duplicate_prompt_preview() {
        let prompts = [
            "wire up exporter for daily reports",
            "refactor the merge-lock retry policy",
            "investigate flaky CI on macOS runners",
        ];
        let titles: [Option<&str>; 3] = [None, None, None];
        let plain = render_popup(&prompts, &titles);

        // The list pane is 40 cols wide and `first_line_truncated` ellipsises
        // anything past ~26 chars, so these 34-char prompts are truncated in
        // the list for every task. The body pane (right half) renders the
        // *selected* task's full prompt untouched. Therefore:
        //   - selected (index 0, since backlog_sel defaults to 0): count == 1
        //   - non-selected: count == 0
        // This still catches both regressions of interest: the body pane
        // silently dropping the prompt (selected → 0) and a list-pane
        // re-introduction of the original doubling bug (selected → 2+).
        for (i, p) in prompts.iter().enumerate() {
            let count = plain.matches(p).count();
            let expected = if i == 0 { 1 } else { 0 };
            assert_eq!(
                count,
                expected,
                "prompt {:?} (selected={}) should appear {} time(s), got {}:\n{}",
                p,
                i == 0,
                expected,
                count,
                plain
            );
        }
        // The pending-title placeholder should appear once per untitled task.
        let pending = plain.matches("pending title").count();
        assert_eq!(
            pending,
            prompts.len(),
            "expected one 'pending title' hint per untitled task:\n{}",
            plain
        );
    }

    #[test]
    fn titled_entries_keep_title_then_id_prompt_layout() {
        let prompts = ["ship the new exporter"];
        let titles = [Some("Exporter rollout")];
        let plain = render_popup(&prompts, &titles);
        assert!(
            plain.contains("Exporter rollout"),
            "title should render on row 1:\n{}",
            plain
        );
        assert!(
            plain.contains("ship the new exporter"),
            "prompt preview should still render on row 2:\n{}",
            plain
        );
        // No pending-title hint when the title has landed.
        assert!(
            !plain.contains("pending title"),
            "titled entry should not show 'pending title' hint:\n{}",
            plain
        );
    }
}

#[cfg(test)]
mod backlog_age_tests {
    use crate::app::App;
    use crate::orchestrator::{now_unix_secs, Project, TaskState, TaskStatus};
    use crate::projects_scan::ProjectsSnapshot;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn backlog_entries_show_right_justified_age_column() {
        let now = now_unix_secs();
        let project = Project {
            id: "p-bk".into(),
            name: "demo".into(),
            root: PathBuf::from("/tmp/demo"),
            created_at: now,
            build_cmd: None,
        };
        let ages: [(i64, &str); 4] = [
            (45, "45s"),
            (12 * 60, "12m"),
            (3 * 3600, "3h"),
            (2 * 86400, "2d"),
        ];
        let tasks: Vec<Arc<TaskState>> = ages
            .iter()
            .enumerate()
            .map(|(i, (age_s, _))| {
                let mut s = TaskState::new(
                    project.id.clone(),
                    project.root.clone(),
                    format!("prompt number {}", i),
                );
                s.status = TaskStatus::Backlog;
                s.created_at = now - age_s;
                Arc::new(s)
            })
            .collect();
        let mut app = App::new();
        let mut by_proj = HashMap::new();
        by_proj.insert(project.id.clone(), tasks);
        let snap = ProjectsSnapshot {
            projects: vec![project],
            tasks: by_proj,
            titling: HashSet::new(),
            merge_lock_holders: HashMap::new(),
            merge_lock_holder_pr_ids: HashMap::new(),
            pr_summaries: HashMap::new(),
        };
        app.update_projects(snap);
        app.open_backlog();

        let backend = TestBackend::new(90, 22);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| super::render_backlog(f, f.area(), &app))
            .expect("render");
        let rendered = buffer_to_string(terminal.backend().buffer());
        println!(
            "---begin backlog render---\n{}---end backlog render---",
            rendered
        );

        for (_, token) in ages.iter() {
            let padded = format!("{:>4}", token);
            assert!(
                rendered.contains(&padded),
                "expected right-justified age {:?} in backlog render:\n{}",
                padded,
                rendered,
            );
        }
    }
}
