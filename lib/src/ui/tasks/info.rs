use super::{ago, wrap_text};
use crate::app::App;
use crate::tasks::store::Artifact;
use crate::ui::artifacts::{
    classify_artifact, evidence_card_header, read_text_excerpt, truncated_footer, CardKind,
};
use crate::ui::common::{centered_rect, popup_block, priority_color};
use crate::ui::now_ms;
use crate::ui::palette::{ACCENT_BLUE, DIM_TEXT, FAINT_TEXT, KIND_TEAL, META_GRAY, TAG_SLATE};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;
use std::path::Path;

/// Body-line budget for one attachment's inline excerpt in the Task Info
/// popup. Fixed and modest — the popup is a reference list, not a reader;
/// `o` opens the full document externally.
const INFO_EXCERPT_LINES: usize = 6;

/// Task Info popup for the focused board card: prompt + metadata up top, then
/// every attachment as a card — text files inline a short excerpt, URLs and
/// media render a one-line pointer. `j`/`k` select, `c` copies the stored
/// path, `o` opens externally, `x` removes, `a` attaches another.
pub(crate) fn render_task_info(frame: &mut Frame, area: Rect, app: &mut App) {
    let popup_area = centered_rect(area, 0.8);
    frame.render_widget(Clear, popup_area);

    let Some(t) = app.selected_board_task().cloned() else {
        frame.render_widget(popup_block(" Task — nothing focused "), popup_area);
        return;
    };

    let title = match t.title.as_deref().filter(|s| !s.is_empty()) {
        Some(name) => format!(
            " Task · {} · {} ",
            crate::tasks::store::short_task_id(&t.task_id),
            name,
        ),
        None => format!(
            " Task · {} ",
            crate::tasks::store::short_task_id(&t.task_id)
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
    if inner.height < 2 || inner.width == 0 {
        return;
    }

    let now_secs = now_ms() / 1000;
    let n_attach = t.artifacts.len();
    let sel = if n_attach == 0 {
        0
    } else {
        app.tasks.info_sel.min(n_attach - 1)
    };

    // ── Canvas: header + prompt + attachment cards, one Line per row ──────
    let (status_icon, status_accent) = crate::ui::common::task_status_meta(t.status);
    let mut header_spans = vec![
        Span::styled(
            format!("{} {}", status_icon, t.status.board_label()),
            Style::default()
                .fg(status_accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            t.priority.label().to_string(),
            Style::default().fg(priority_color(t.priority)),
        ),
        Span::raw("  "),
        Span::styled(ago(now_secs, t.created_at), Style::default().fg(META_GRAY)),
        Span::raw("  "),
        Span::styled(
            format!(
                "📎 {} attachment{}",
                n_attach,
                if n_attach == 1 { "" } else { "s" }
            ),
            Style::default().fg(Color::Rgb(150, 130, 200)),
        ),
    ];
    if let Some(kind) = t.kind.as_deref() {
        header_spans.push(Span::raw("  "));
        header_spans.push(Span::styled(
            format!("⛭ {}", kind),
            Style::default().fg(KIND_TEAL),
        ));
    }
    if !t.tags.is_empty() {
        let tags = t
            .tags
            .iter()
            .map(|tag| format!("#{}", tag))
            .collect::<Vec<_>>()
            .join(" ");
        header_spans.push(Span::raw("  "));
        header_spans.push(Span::styled(tags, Style::default().fg(TAG_SLATE)));
    }
    let mut lines: Vec<Line<'static>> = vec![Line::from(header_spans), Line::raw("")];

    let stats_text = match &t.stats {
        Some(stats) => format!(
            "Task Stats · {} tokens · {} · {} sessions\nInput {} · Output {} · Cache read {} · Cache write {}",
            stats.total_tokens(), stats.cost_label(), stats.sessions, stats.input_tokens, stats.output_tokens,
            stats.cache_read_tokens, stats.cache_creation_tokens,
        ),
        None => "Task Stats · usage unavailable (awaiting session transcripts)".into(),
    };
    for line in stats_text.lines() {
        for segment in wrap_text(line, inner.width as usize) {
            lines.push(Line::from(Span::styled(
                segment,
                Style::default().fg(META_GRAY),
            )));
        }
    }
    lines.push(Line::raw(""));
    // Keep room for attachments below the task prompt.
    let prompt_lines = wrap_text(&t.prompt, inner.width as usize);
    let capped = prompt_lines.len() > 8;
    for seg in prompt_lines.into_iter().take(8) {
        lines.push(Line::from(Span::styled(
            seg,
            Style::default().fg(Color::Rgb(200, 200, 210)),
        )));
    }
    if capped {
        lines.push(Line::from(Span::styled("…", Style::default().fg(DIM_TEXT))));
    }
    lines.push(Line::raw(""));

    // ── Attachment cards ──────────────────────────────────────────────────
    // Each card's (top, end) canvas rows, so the scroll clamp can keep the
    // selected card in view.
    let mut card_spans: Vec<(u16, u16)> = Vec::with_capacity(n_attach);
    if n_attach == 0 {
        lines.push(Line::from(Span::styled(
            "  (no attachments — a attaches a file or URL, p pastes the clipboard as a note)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    for (i, a) in t.artifacts.iter().enumerate() {
        let top = lines.len() as u16;
        let is_lead = t.lead_artifact == Some(i);
        lines.push(evidence_card_header(a, i == sel, is_lead));
        lines.extend(attachment_body(a));
        lines.push(Line::raw(""));
        card_spans.push((top, lines.len() as u16));
    }

    // ── Scroll clamp + draw ───────────────────────────────────────────────
    let body_h = inner.height - 1;
    let total = lines.len() as u16;
    let mut scroll = app.render.task_info_scroll;
    if let Some(&(top, end)) = card_spans.get(sel) {
        let h = end.saturating_sub(top);
        if top < scroll {
            scroll = top;
        } else if top + h > scroll + body_h {
            scroll = (top + h).saturating_sub(body_h);
        }
    }
    scroll = scroll.min(total.saturating_sub(body_h));
    app.render.task_info_scroll = scroll;

    let body_area = Rect::new(inner.x, inner.y, inner.width, body_h);
    // No wrap: the scroll math above assumes one visual row per canvas line;
    // over-long excerpt/URL lines clip instead of pushing content down.
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), body_area);

    let footer_area = Rect::new(inner.x, inner.y + body_h, inner.width, 1);
    let pos = if n_attach == 0 {
        "attachment —".to_string()
    } else {
        format!("attachment {}/{}", sel + 1, n_attach)
    };
    let hint = format!(
        " {}   j/k:select   a:attach   p:paste note   c:copy path   o:open   x:remove   esc/v:close ",
        pos
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        ))),
        footer_area,
    );
}

/// An attachment card's body below its header: a text excerpt, or a
/// one-line pointer for URLs and media.
fn attachment_body(a: &Artifact) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match classify_artifact(a) {
        CardKind::Text | CardKind::Diff => {
            let path = Path::new(&a.path);
            match read_text_excerpt(path, 8 * 1024) {
                None => {
                    let msg = if std::fs::metadata(path).is_ok() {
                        Span::styled(
                            "  (binary file — open externally with `o`)",
                            Style::default().fg(Color::DarkGray),
                        )
                    } else {
                        Span::styled(
                            format!("  (cannot read {})", a.path),
                            Style::default().fg(Color::Rgb(220, 100, 100)),
                        )
                    };
                    lines.push(Line::from(msg));
                }
                Some((content, truncated)) => {
                    let shown = content.len().min(INFO_EXCERPT_LINES);
                    for s in content.iter().take(shown) {
                        lines.push(Line::from(Span::styled(
                            format!("  {}", s),
                            Style::default().fg(Color::Gray),
                        )));
                    }
                    let hidden = content.len().saturating_sub(shown) + truncated;
                    if hidden > 0 {
                        lines.push(truncated_footer(hidden));
                    }
                }
            }
        }
        CardKind::Url => {
            lines.push(Line::from(Span::styled(
                format!("  {}", a.path),
                Style::default().fg(ACCENT_BLUE),
            )));
        }
        CardKind::Image => {
            lines.push(Line::from(Span::styled(
                "  (image — press `o` to open)",
                Style::default().fg(Color::DarkGray),
            )));
        }
        CardKind::Video => {
            lines.push(Line::from(Span::styled(
                "  (video — press `o` to open)",
                Style::default().fg(Color::DarkGray),
            )));
        }
        CardKind::Fallback => {
            lines.push(Line::from(Span::styled(
                format!("  {}", a.path),
                Style::default().fg(FAINT_TEXT),
            )));
        }
    }
    lines
}

// Unix-only: these construct an App over the on-disk personal store, which
// with_temp_home isolates by redirecting $HOME (unix-only mechanism).
#[cfg(all(test, unix))]
mod task_info_tests {
    use crate::app::App;
    use crate::test_util::with_temp_home;
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn info_popup_inlines_excerpt_url_and_footer() {
        with_temp_home(|| {
            let mut app = App::new();
            let id = app.tasks.board.add("write the report").unwrap().unwrap();
            let dir = std::env::temp_dir().join(format!("cchub-info-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let doc = dir.join("notes.md");
            std::fs::write(&doc, "alpha finding\nbeta finding\n").unwrap();
            crate::ops::task::task_artifact_add(
                &id,
                doc.to_str().unwrap(),
                None,
                Some("research notes".into()),
                false,
            )
            .unwrap();
            crate::ops::task::task_artifact_add(&id, "https://example.com/spec", None, None, false)
                .unwrap();
            app.tasks.reload();
            app.focus_task(&id);
            assert!(app.enter_task_info());

            let backend = TestBackend::new(100, 30);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| super::render_task_info(f, f.area(), &mut app))
                .expect("render");
            let dump = buffer_to_string(terminal.backend().buffer());
            assert!(
                dump.contains("write the report"),
                "prompt visible\n{}",
                dump
            );
            assert!(
                dump.contains("alpha finding"),
                "text excerpt inlined\n{}",
                dump
            );
            assert!(
                dump.contains("research notes"),
                "caption on card header\n{}",
                dump
            );
            assert!(
                dump.contains("https://example.com/spec"),
                "url card shows the URL\n{}",
                dump
            );
            assert!(dump.contains("attachment 1/2"), "footer position\n{}", dump);
            std::fs::remove_dir_all(&dir).ok();
        });
    }

    #[test]
    fn info_popup_without_attachments_hints_attach_key() {
        with_temp_home(|| {
            let mut app = App::new();
            let id = app.tasks.board.add("bare card").unwrap().unwrap();
            app.focus_task(&id);
            assert!(app.enter_task_info());
            let backend = TestBackend::new(80, 24);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| super::render_task_info(f, f.area(), &mut app))
                .expect("render");
            let dump = buffer_to_string(terminal.backend().buffer());
            assert!(
                dump.contains("no attachments"),
                "empty state hint expected\n{}",
                dump
            );
        });
    }
}
