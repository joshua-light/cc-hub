//! The progressive-disclosure "Result" popup for the selected Projects task.

use crate::app::App;
use crate::models;
use crate::ui::common::{centered_rect, popup_block};
use crate::ui::now_ms;
use crate::ui::palette::{BACKLOG_BLUE, FAINT_PURPLE_GRAY};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;
use ratatui_image::StatefulImage;

use super::{
    artifact_preview_total_lines, card_body_height, classify_artifact, ensure_image_decoded,
    evidence_card_header, lead_card_body_height, render_diff_card_body, render_fallback_card_body,
    render_image_placeholder, render_text_card_body, render_url_card_body, render_video_card_body,
    shipped_version_span, CardKind,
};

/// "Result" popup for the selected Projects task. Progressive-disclosure
/// layout: header (status · age · count) → one-line note (proof headline) →
/// lead artifact (large) → other artifacts → muted "summary" appendix at the
/// bottom. The note is the headline; the agent's full summary is an appendix
/// the user scrolls into, not the centerpiece.
pub(crate) fn render_projects_result(frame: &mut Frame, area: Rect, app: &mut App) {
    let popup_area = centered_rect(area, 0.85);
    frame.render_widget(Clear, popup_area);

    let Some(t) = app.selected_project_task().cloned() else {
        frame.render_widget(popup_block(" Result — no task selected "), popup_area);
        return;
    };

    let status_label = t.status.as_str();
    let status_color = match t.status {
        crate::orchestrator::TaskStatus::Running => Color::LightYellow,
        crate::orchestrator::TaskStatus::Review => Color::LightCyan,
        crate::orchestrator::TaskStatus::Merging => Color::LightMagenta,
        crate::orchestrator::TaskStatus::Done => Color::LightGreen,
        crate::orchestrator::TaskStatus::Backlog => BACKLOG_BLUE,
    };
    let title = match t.title.as_deref().filter(|s| !s.is_empty()) {
        Some(name) => format!(
            " Result · {} · {} ",
            crate::orchestrator::short_task_id(&t.task_id),
            name,
        ),
        None => format!(
            " Result · {} ",
            crate::orchestrator::short_task_id(&t.task_id)
        ),
    };
    let block = popup_block(Span::styled(
        title,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height == 0 {
        return;
    }

    let now_secs = now_ms() / 1000;
    let age = models::relative_age(now_secs.saturating_sub(t.updated_at as u64));

    // ── Header (status badge · age · count) + note headline ────────────────
    let mut header_lines: Vec<Line<'static>> = Vec::new();
    let mut header_spans = vec![
        Span::styled(
            format!("[{}]", status_label),
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(age, Style::default().fg(FAINT_PURPLE_GRAY)),
    ];
    if let Some(v) = t.shipped_version.as_deref().filter(|s| !s.is_empty()) {
        header_spans.push(Span::raw("  "));
        header_spans.push(shipped_version_span(v));
    }
    header_spans.push(Span::raw("  "));
    header_spans.push(Span::styled(
        format!(
            "({} artifact{})",
            t.artifacts.len(),
            if t.artifacts.len() == 1 { "" } else { "s" }
        ),
        Style::default().fg(Color::Rgb(150, 130, 200)),
    ));
    header_lines.push(Line::from(header_spans));
    header_lines.push(Line::raw(""));

    // Note is the headline proof; falls back to a single-line truncation of
    // the prompt when the orchestrator hasn't written one yet.
    let note_text: String = match t.note.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) => n.lines().next().unwrap_or("").to_string(),
        None => t.prompt.lines().next().unwrap_or("").trim().to_string(),
    };
    if !note_text.is_empty() {
        header_lines.push(Line::from(Span::styled(
            note_text,
            Style::default()
                .fg(Color::Rgb(220, 220, 230))
                .add_modifier(Modifier::ITALIC),
        )));
        header_lines.push(Line::raw(""));
    }

    // Lead artifact renders first; the rest follow in original order.
    let lead_idx = t.lead_artifact.filter(|&i| i < t.artifacts.len());
    let render_order: Vec<usize> = lead_idx
        .into_iter()
        .chain((0..t.artifacts.len()).filter(|i| Some(*i) != lead_idx))
        .collect();

    // ── Layout & scrolling ────────────────────────────────────────────────
    let header_h = header_lines.len() as u16;
    let body_h = inner.height.saturating_sub(1);
    let body_area = Rect::new(inner.x, inner.y, inner.width, body_h);
    let footer_area = Rect::new(inner.x, inner.y + body_h, inner.width, 1);

    // When the user has hit `e`, the selected card swells to fill most of
    // the visible body area; non-selected cards keep their default heights so
    // the surrounding context stays in view.
    let expanded_body_h: u16 = if app.render.result_artifact_expanded {
        body_h.saturating_sub(6).min(40)
    } else {
        0
    };
    let mut card_meta: Vec<(usize, CardKind, u16)> = Vec::with_capacity(render_order.len());
    for &art_idx in &render_order {
        let kind = classify_artifact(&t.artifacts[art_idx]);
        let default_h = if lead_idx == Some(art_idx) {
            lead_card_body_height(kind)
        } else {
            card_body_height(kind)
        };
        let h =
            if app.render.result_artifact_expanded && art_idx == app.projects.result_artifact_sel {
                expanded_body_h.max(default_h)
            } else {
                default_h
            };
        card_meta.push((art_idx, kind, h));
    }

    let mut canvas_card_tops: Vec<u16> = Vec::with_capacity(card_meta.len());
    let mut next_y = header_h;
    for (_, _, body) in &card_meta {
        canvas_card_tops.push(next_y);
        // Card = header(1) + body + spacer(1).
        next_y = next_y.saturating_add(1 + body + 1);
    }
    if t.artifacts.is_empty() {
        next_y = next_y.saturating_add(1); // "(no artifacts)" line
    }

    let summary_lines: Vec<Line<'static>> = match t
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => Vec::new(),
        Some(s) => {
            let mut out: Vec<Line<'static>> = Vec::new();
            out.push(Line::raw(""));
            out.push(Line::from(Span::styled(
                "summary",
                Style::default().fg(Color::Rgb(120, 120, 140)),
            )));
            for line in s.lines() {
                out.push(Line::from(Span::styled(
                    format!("  {}", line),
                    Style::default().fg(FAINT_PURPLE_GRAY),
                )));
            }
            out
        }
    };
    let summary_h = summary_lines.len() as u16;
    next_y = next_y.saturating_add(summary_h);
    let total_canvas_h = next_y;

    let expanded_scroll_budget = if app.render.result_artifact_expanded {
        card_meta
            .iter()
            .find(|(art_idx, _, _)| *art_idx == app.projects.result_artifact_sel)
            .and_then(|(art_idx, kind, body)| {
                let max_bytes = 64 * 1024;
                // Count at the card body's render width (`inner.width - 2`,
                // see `body_rect` below). Today `build_diff_lines` emits one
                // row per diff line at any width, so this only matters if a
                // renderer ever starts wrapping — but counting at a different
                // width than the render is exactly the class of drift that
                // broke the popup scroll math before.
                let body_width = inner.width.saturating_sub(2);
                artifact_preview_total_lines(&t.artifacts[*art_idx], *kind, max_bytes, body_width)
                    .map(|total| total.saturating_sub(*body as usize).min(u16::MAX as usize) as u16)
            })
            .unwrap_or(0)
    } else {
        0
    };

    // Auto-scroll so the selected card stays on-screen — but only while
    // collapsed. In expanded mode the user is deliberately scrolling INTO the
    // selected card's long excerpt (header off the top is expected), and this
    // frame-recurrent snap would pin `result_scroll ≤ sel_top`, making the
    // expanded overscroll budget below unreachable for any card that isn't
    // already at the canvas bottom.
    if !t.artifacts.is_empty() && body_h > 0 && !app.render.result_artifact_expanded {
        let sel_art_idx = app.projects.result_artifact_sel.min(t.artifacts.len() - 1);
        let sel_render_pos = render_order
            .iter()
            .position(|&i| i == sel_art_idx)
            .unwrap_or(0);
        let sel_top = canvas_card_tops[sel_render_pos];
        let (_, _, sel_body) = card_meta[sel_render_pos];
        let sel_h = 1 + sel_body + 1;
        if sel_top < app.render.result_scroll {
            app.render.result_scroll = sel_top;
        } else if sel_top + sel_h > app.render.result_scroll + body_h {
            app.render.result_scroll = sel_top + sel_h - body_h;
        }
    }
    let base_max_scroll = total_canvas_h.saturating_sub(body_h);
    let max_scroll = base_max_scroll.saturating_add(expanded_scroll_budget);
    if app.render.result_scroll > max_scroll {
        app.render.result_scroll = max_scroll;
    }
    let scroll = app.render.result_scroll;
    // The canvas itself pins at its own max; anything past that is the
    // expanded card's overscroll, consumed by the card body's internal scroll
    // (`body_scroll_lines` below). Feeding raw overscroll into the Paragraph
    // and the overlay positions would push the whole canvas — card included —
    // off the top, leaving the popup blank instead of revealing deeper lines.
    let canvas_scroll = scroll.min(base_max_scroll);

    // The placeholder blank rows below each card header keep y-offsets honest
    // for the Paragraph's vertical scroll; per-card widgets paint over them in
    // the second pass.
    let mut canvas_lines: Vec<Line<'static>> = Vec::with_capacity(total_canvas_h as usize);
    canvas_lines.extend(header_lines);
    if t.artifacts.is_empty() {
        canvas_lines.push(Line::from(Span::styled(
            "  (no artifacts attached)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for &(art_idx, _, body) in &card_meta {
            let a = &t.artifacts[art_idx];
            let selected = art_idx == app.projects.result_artifact_sel;
            let is_lead = lead_idx == Some(art_idx);
            canvas_lines.push(evidence_card_header(a, selected, is_lead));
            for _ in 0..body {
                canvas_lines.push(Line::raw(""));
            }
            canvas_lines.push(Line::raw(""));
        }
    }
    canvas_lines.extend(summary_lines);
    // No wrap: `canvas_card_tops` and the overlay math below assume one visual
    // row per canvas line. With `Wrap`, any over-long line (a long note, an
    // artifact caption in the card header, a summary line) added extra wrapped
    // rows that shoved the real content down while the overlays stayed put,
    // painting card bodies over the wrong rows. Clipping keeps 1 line == 1 row.
    let canvas_para = Paragraph::new(canvas_lines).scroll((canvas_scroll, 0));
    frame.render_widget(canvas_para, body_area);

    // Per-card widgets, painted on top of the placeholder rows.
    for (rp, &(art_idx, kind, body)) in card_meta.iter().enumerate() {
        let a = &t.artifacts[art_idx];
        let card_top_canvas = canvas_card_tops[rp];
        let body_top_canvas = card_top_canvas + 1;
        let body_screen_top = body_area.y as i32 + body_top_canvas as i32 - canvas_scroll as i32;
        let body_screen_bot = body_screen_top + body as i32;
        let view_top = body_area.y as i32;
        let view_bot = (body_area.y + body_h) as i32;
        if body_screen_bot <= view_top || body_screen_top >= view_bot {
            continue;
        }
        let visible_top = body_screen_top.max(view_top);
        let visible_bot = body_screen_bot.min(view_bot);
        let visible_h = (visible_bot - visible_top).max(0) as u16;
        if visible_h == 0 {
            continue;
        }
        let body_rect = Rect::new(
            body_area.x.saturating_add(2),
            visible_top as u16,
            body_area.width.saturating_sub(2),
            visible_h,
        );
        let clipped_scroll = if body_screen_top < view_top {
            (view_top - body_screen_top) as usize
        } else {
            0
        };
        let overscroll_lines = scroll.saturating_sub(base_max_scroll) as usize;
        let body_scroll_lines = clipped_scroll.saturating_add(overscroll_lines);
        match kind {
            CardKind::Image => {
                // Kitty/sixel/iterm2 protocols write pixel data tied to a fixed
                // rect; partially-clipped rects leave terminal residue when the
                // popup scrolls. Only render when fully visible.
                let fully_visible = body_screen_top >= view_top && body_screen_bot <= view_bot;
                if !fully_visible {
                    render_image_placeholder(frame, body_rect, "[image hidden — scroll to view]");
                    continue;
                }
                if app.image_picker.is_none() {
                    render_image_placeholder(
                        frame,
                        body_rect,
                        "[image preview unavailable — terminal doesn't support graphics]",
                    );
                    continue;
                }
                let path = a.path.clone();
                if !ensure_image_decoded(app, &path) {
                    render_image_placeholder(
                        frame,
                        body_rect,
                        "[image preview unavailable — decode failed; press `o` to open]",
                    );
                    continue;
                }
                if let Some(state) = app.render.artifact_images.get_mut(&path) {
                    let widget =
                        StatefulImage::<ratatui_image::protocol::StatefulProtocol>::default();
                    frame.render_stateful_widget(widget, body_rect, state);
                }
            }
            CardKind::Text => {
                let expanded = app.render.result_artifact_expanded
                    && art_idx == app.projects.result_artifact_sel;
                let max_bytes = if expanded { 64 * 1024 } else { 8 * 1024 };
                render_text_card_body(frame, body_rect, a, max_bytes, body_scroll_lines);
            }
            CardKind::Diff => {
                let expanded = app.render.result_artifact_expanded
                    && art_idx == app.projects.result_artifact_sel;
                let max_bytes = if expanded { 64 * 1024 } else { 8 * 1024 };
                render_diff_card_body(frame, body_rect, a, max_bytes, body_scroll_lines);
            }
            CardKind::Video => render_video_card_body(frame, body_rect, a),
            CardKind::Url => render_url_card_body(frame, body_rect, a),
            CardKind::Fallback => render_fallback_card_body(frame, body_rect, a),
        }
    }

    let artifact_pos = if t.artifacts.is_empty() {
        "artifact —".to_string()
    } else {
        format!(
            "artifact {}/{}",
            app.projects.result_artifact_sel.min(t.artifacts.len() - 1) + 1,
            t.artifacts.len()
        )
    };
    let hint = format!(
        " {}   esc/r:close   j/k:artifact   e:expand   PgUp/PgDn:scroll   c:copy path   o:xdg-open ",
        artifact_pos
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        ))),
        footer_area,
    );
}

#[cfg(test)]
mod result_popup_tests {
    use crate::app::App;
    use crate::orchestrator::{Artifact, Project, TaskState, TaskStatus};
    use crate::projects_scan::ProjectsSnapshot;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// Returns the y-coordinate of the first row that contains `needle`, or
    /// `None` when the substring is missing. Used to assert the proof-first
    /// vertical ordering of the redesigned popup (note → lead → others →
    /// summary appendix).
    fn row_of(buf: &Buffer, needle: &str) -> Option<u16> {
        for y in 0..buf.area().height {
            let mut row = String::new();
            for x in 0..buf.area().width {
                row.push_str(buf[(x, y)].symbol());
            }
            if row.contains(needle) {
                return Some(y);
            }
        }
        None
    }

    #[test]
    fn expanded_text_artifact_scrolls_with_popup_scroll() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let doc_path = tmp.path().join("tour.md");
        let body = (1..=80)
            .map(|n| format!("line {:02}", n))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&doc_path, body).unwrap();

        let now = crate::orchestrator::now_unix_secs();
        let project = Project {
            id: "p-scroll".into(),
            name: "scroll".into(),
            root: PathBuf::from("/tmp/scroll"),
            created_at: now,
            build_cmd: None,
        };
        let mut state = TaskState::new(
            project.id.clone(),
            project.root.clone(),
            "scroll prompt".into(),
        );
        state.status = TaskStatus::Done;
        state.note = Some("headline".into());
        state.artifacts = vec![Artifact {
            kind: "file".into(),
            path: doc_path.to_string_lossy().into_owned(),
            original: doc_path.to_string_lossy().into_owned(),
            caption: Some("long doc".into()),
            added_at: now,
        }];
        state.lead_artifact = Some(0);

        let mut app = App::new();
        let mut tasks = HashMap::new();
        tasks.insert(project.id.clone(), vec![std::sync::Arc::new(state)]);
        let snap = ProjectsSnapshot {
            projects: vec![project],
            tasks,
            titling: std::collections::HashSet::new(),
            merge_lock_holders: std::collections::HashMap::new(),
            merge_lock_holder_pr_ids: std::collections::HashMap::new(),
            pr_summaries: std::collections::HashMap::new(),
        };
        app.update_projects(snap);
        assert!(app.enter_projects_result(), "popup should open");
        app.render.result_artifact_expanded = true;

        let backend = TestBackend::new(100, 28);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| super::render_projects_result(f, f.area(), &mut app))
            .expect("render");
        let first = buffer_to_string(terminal.backend().buffer());
        assert!(
            first.contains("line 01"),
            "top of doc should be visible\n{}",
            first
        );

        app.result_scroll_by(30);
        terminal
            .draw(|f| super::render_projects_result(f, f.area(), &mut app))
            .expect("render");
        let second = buffer_to_string(terminal.backend().buffer());
        assert!(
            second.contains("line 29") || second.contains("line 30"),
            "expanded overscroll should advance the excerpt ~1:1 with the popup scroll\n{}",
            second
        );
        assert!(
            !second.contains("line 01"),
            "once scrolled down, the preview should not stay pinned to the first line\n{}",
            second
        );

        // The end of the excerpt must be reachable: a huge scroll clamps to
        // the expanded budget and lands on the last line, not short of it.
        app.result_scroll_by(1000);
        terminal
            .draw(|f| super::render_projects_result(f, f.area(), &mut app))
            .expect("render");
        let third = buffer_to_string(terminal.backend().buffer());
        assert!(
            third.contains("line 80"),
            "max overscroll should reveal the final line of the excerpt\n{}",
            third
        );
    }

    #[test]
    fn evidence_inlines_log_url_and_image_fallback() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log_path = tmp.path().join("build.log");
        std::fs::write(
            &log_path,
            "compiling cc-hub-lib\nfinished release in 12.3s\n",
        )
        .unwrap();

        let now = crate::orchestrator::now_unix_secs();
        let project = Project {
            id: "p-test".into(),
            name: "test".into(),
            root: PathBuf::from("/tmp/test"),
            created_at: now,
            build_cmd: None,
        };
        let mut state = TaskState::new(
            project.id.clone(),
            project.root.clone(),
            "test prompt body".into(),
        );
        state.status = TaskStatus::Done;
        state.note = Some("PROOF-LINE: build is green".into());
        state.summary = Some("WHY this works: build green; popup shows evidence cards.".into());
        state.artifacts = vec![
            Artifact {
                kind: "build".into(),
                path: log_path.to_string_lossy().into_owned(),
                original: log_path.to_string_lossy().into_owned(),
                caption: Some("cargo build --release".into()),
                added_at: now,
            },
            Artifact {
                kind: "url".into(),
                path: "https://example.com/ci/build/42".into(),
                original: "https://example.com/ci/build/42".into(),
                caption: Some("CI build".into()),
                added_at: now,
            },
            Artifact {
                kind: "screenshot".into(),
                path: "/nonexistent/missing-screenshot.png".into(),
                original: "/nonexistent/missing-screenshot.png".into(),
                caption: Some("preview".into()),
                added_at: now,
            },
        ];
        // Designate the build log as the lead artifact so it renders first
        // and at the lead body height.
        state.lead_artifact = Some(0);

        let mut app = App::new();
        let mut tasks = HashMap::new();
        tasks.insert(project.id.clone(), vec![std::sync::Arc::new(state)]);
        let snap = ProjectsSnapshot {
            projects: vec![project],
            tasks,
            titling: std::collections::HashSet::new(),
            merge_lock_holders: std::collections::HashMap::new(),
            merge_lock_holder_pr_ids: std::collections::HashMap::new(),
            pr_summaries: std::collections::HashMap::new(),
        };
        app.update_projects(snap);
        assert!(app.enter_projects_result(), "popup should open");

        // Tall buffer so the entire canvas (note → lead → others → summary
        // appendix) fits without scrolling — the test asserts vertical order,
        // so everything must be visible in one frame.
        let backend = TestBackend::new(120, 60);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| super::render_projects_result(f, f.area(), &mut app))
            .expect("render");

        let buf = terminal.backend().buffer().clone();
        let dump = buffer_to_string(&buf);

        assert!(dump.contains("Result"), "should render Result title");
        assert!(
            dump.contains("PROOF-LINE: build is green"),
            "note headline should appear above evidence\n{}",
            dump
        );
        assert!(
            dump.contains("WHY this works"),
            "summary text should be inlined as appendix\n{}",
            dump
        );
        assert!(
            dump.contains("cargo build"),
            "log card should inline file caption\n{}",
            dump
        );
        assert!(
            dump.contains("compiling cc-hub-lib"),
            "log card body should inline file content\n{}",
            dump
        );
        assert!(
            dump.contains("lead"),
            "lead artifact card header should carry a `lead` tag\n{}",
            dump
        );
        assert!(
            dump.contains("press"),
            "url/video card should hint at `o`\n{}",
            dump
        );
        assert!(
            dump.contains("https://example.com/ci/build/42"),
            "url card should show URL\n{}",
            dump
        );
        // Image with no decoded data and no picker → falls back to a placeholder
        // (one of the two messages the renderer emits).
        assert!(
            dump.contains("[image preview unavailable") || dump.contains("[image hidden"),
            "image fallback placeholder should appear\n{}",
            dump
        );

        // Vertical ordering: note → lead body → other artifact body → summary.
        let y_note = row_of(&buf, "PROOF-LINE").expect("note row");
        let y_lead_body = row_of(&buf, "compiling cc-hub-lib").expect("lead body row");
        let y_url = row_of(&buf, "https://example.com").expect("url body row");
        let y_summary = row_of(&buf, "WHY this works").expect("summary appendix row");
        assert!(
            y_note < y_lead_body && y_lead_body < y_url && y_url < y_summary,
            "expected order note({}) < lead({}) < url({}) < summary({})\n{}",
            y_note,
            y_lead_body,
            y_url,
            y_summary,
            dump,
        );
    }

    fn buffer_bg_map(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                let c = &buf[(x, y)];
                let m = match c.bg {
                    ratatui::style::Color::Rgb(34, 92, 43) => '+',
                    ratatui::style::Color::Rgb(122, 41, 54) => '-',
                    ratatui::style::Color::Reset => '.',
                    _ => '?',
                };
                out.push(m);
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn diff_artifact_renders_with_claude_style_backgrounds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let patch_path = tmp.path().join("sample.patch");
        let patch = "\
diff --git a/lib/src/ui.rs b/lib/src/ui.rs
index 0000001..0000002 100644
--- a/lib/src/ui.rs
+++ b/lib/src/ui.rs
@@ -42,3 +42,5 @@ fn render_status() {
     let mut spans = Vec::new();
-    spans.push(span_dim(&state.title));
+    spans.push(span_bold(&state.title));
+    if let Some(badge) = &state.badge {
+        spans.push(span_subtle(badge));
+    }
     Line::from(spans)
@@ -101,3 +103,3 @@ fn render_bar() {
     let bar = renderer.bar();
-    bar.draw(area);
+    bar.draw_with_offset(area, offset);
     Ok(())
";
        std::fs::write(&patch_path, patch).unwrap();

        let now = crate::orchestrator::now_unix_secs();
        let project = Project {
            id: "p-diff".into(),
            name: "diff".into(),
            root: PathBuf::from("/tmp/diff"),
            created_at: now,
            build_cmd: None,
        };
        let mut state = TaskState::new(
            project.id.clone(),
            project.root.clone(),
            "diff prompt".into(),
        );
        state.status = TaskStatus::Done;
        state.summary = Some("WHY: diff renderer matches Claude Code style.".into());
        state.artifacts = vec![Artifact {
            kind: "diff".into(),
            path: patch_path.to_string_lossy().into_owned(),
            original: patch_path.to_string_lossy().into_owned(),
            caption: Some("ui.rs: structured diff".into()),
            added_at: now,
        }];

        let mut app = App::new();
        let mut tasks = HashMap::new();
        tasks.insert(project.id.clone(), vec![std::sync::Arc::new(state)]);
        let snap = ProjectsSnapshot {
            projects: vec![project],
            tasks,
            titling: std::collections::HashSet::new(),
            merge_lock_holders: std::collections::HashMap::new(),
            merge_lock_holder_pr_ids: std::collections::HashMap::new(),
            pr_summaries: std::collections::HashMap::new(),
        };
        app.update_projects(snap);
        assert!(app.enter_projects_result(), "popup should open");

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| super::render_projects_result(f, f.area(), &mut app))
            .expect("render");

        let buf = terminal.backend().buffer().clone();
        let plain = buffer_to_string(&buf);
        let bg_map = buffer_bg_map(&buf);
        assert!(
            plain.contains("Result"),
            "should render Result title\n{}",
            plain
        );
        assert!(
            plain.contains("lib/src/ui.rs"),
            "diff per-file header path should be visible\n{}",
            plain
        );
        assert!(
            plain.contains("Added") && plain.contains("removed"),
            "diff header counts line should be visible\n{}",
            plain
        );
        assert!(
            !plain.contains("@@ -"),
            "raw @@ hunk header should be suppressed\n{}",
            plain
        );
        assert!(
            bg_map.contains('+'),
            "added rows should paint the diffAdded bg across cells\n{}",
            bg_map
        );
        assert!(
            bg_map.contains('-'),
            "removed rows should paint the diffRemoved bg across cells\n{}",
            bg_map
        );
        assert!(
            bg_map.contains("..."),
            "hunk separator marker should render in dim gray\n{}",
            plain
        );
    }
}
