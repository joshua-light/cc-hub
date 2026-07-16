//! Kanban task-card renderers: the rich active card (Planning/Running) and the
//! compact collapsed card (Review/Merging/Done), plus the merge-lock banner and
//! shared header/version span helpers.

use crate::models;
use crate::models::SessionInfo;
use crate::ui::common::{format_duration_secs, short_tool};
use crate::ui::palette::{FAINT_TEXT, LABEL_GRAY, META_GRAY, PURPLE};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::{
    agent_dot_strip, collect_agent_summary, ctx_bar, ctx_color, merge_progress_spans,
    pr_badge_spans, sum_tool_uses, todos_progress, worker_was_merged, TASK_META_DIM,
};

/// Sessions-style rich card for a Running task. Mirrors the layout of the
/// Sessions grid card: bordered, multi-row, with status emoji, agent dots,
/// merge glyph, ctx bar, and live tool/thinking line.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_task_card_active(
    frame: &mut Frame,
    area: Rect,
    t: &crate::orchestrator::TaskState,
    selected: bool,
    col_idx: usize,
    sessions_by_tmux: &std::collections::HashMap<&str, &SessionInfo>,
    pr_summary: Option<&crate::projects_scan::PrCardSummary>,
    now_secs: u64,
    titling_in_flight: bool,
) {
    let sum = collect_agent_summary(t, sessions_by_tmux);

    // Planning vs Running share the rich layout but differ in accent +
    // title icon so the column matches the card visually.
    let (accent, title_icon) = if col_idx == 0 {
        (PURPLE, "󰟶")
    } else {
        (Color::LightYellow, "󰒓")
    };
    let (border_type, border_color) = if selected {
        (BorderType::Double, Color::White)
    } else if sum.waiting > 0 {
        (BorderType::Rounded, Color::Rgb(220, 200, 120))
    } else {
        (BorderType::Rounded, Color::Rgb(80, 90, 110))
    };

    let short_id = crate::orchestrator::short_task_id(&t.task_id);
    let prompt_max = (area.width as usize).saturating_sub(8);
    let header_text = task_card_header_text(t, titling_in_flight, prompt_max);
    let title_spans = vec![
        Span::styled(format!(" {} ", title_icon), Style::default().fg(accent)),
        Span::styled(
            header_text,
            Style::default()
                .fg(if selected { Color::White } else { Color::Gray })
                .add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
        Span::raw(" "),
    ];
    let title = Line::from(title_spans);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(border_color))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width < 4 {
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(inner.height as usize);

    // Row 1: note (orchestrator status) or first line of summary fallback.
    let note_text = t
        .note
        .as_deref()
        .map(|n| models::first_line_truncated(n, inner.width.saturating_sub(4) as usize))
        .unwrap_or_else(|| {
            let s = t.summary.as_deref().unwrap_or("");
            models::first_line_truncated(s, inner.width.saturating_sub(4) as usize)
        });
    if !note_text.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("󰍡 ", Style::default().fg(Color::Rgb(150, 170, 200))),
            Span::styled(note_text, Style::default().fg(Color::Rgb(190, 190, 210))),
        ]));
    } else {
        lines.push(Line::from(Span::raw("")));
    }

    // PR badge row — inserted above agent dots so a `changes requested`
    // bounce-back is obvious before the eye scans the rest of the card.
    // The live-tool row at the bottom is allowed to fall off when present
    // (the card height is fixed at 4 inner rows).
    if let Some(pr) = pr_summary {
        lines.push(Line::from(pr_badge_spans(pr)));
    }

    // Row 2: agent dot strip + merge glyph.
    let mut row2: Vec<Span<'static>> = Vec::new();
    row2.push(Span::styled("agents ", Style::default().fg(LABEL_GRAY)));
    row2.extend(agent_dot_strip(&sum));
    row2.push(Span::raw("   "));
    row2.extend(merge_progress_spans(t));
    lines.push(Line::from(row2));

    let age = models::relative_age(now_secs.saturating_sub(t.updated_at as u64));
    let arts = t.artifacts.len();
    let mut row3: Vec<Span<'static>> = vec![
        Span::styled(
            format!("#{}  ", short_id),
            Style::default().fg(TASK_META_DIM),
        ),
        Span::styled("󰔟 ", Style::default().fg(LABEL_GRAY)),
        Span::styled(age, Style::default().fg(FAINT_TEXT)),
    ];
    if arts > 0 {
        row3.push(Span::styled(
            format!("   󰉂 {}", arts),
            Style::default().fg(Color::Rgb(180, 160, 220)),
        ));
    }
    if let Some((done, total)) = todos_progress(t) {
        row3.push(Span::styled(
            format!("   ☑ {}/{}", done, total),
            Style::default().fg(FAINT_TEXT),
        ));
    }
    if sum.tool_uses > 0 {
        row3.push(Span::styled(
            format!("   󰖷 {}", sum.tool_uses),
            Style::default().fg(Color::Rgb(180, 200, 160)),
        ));
    }
    let left_w: usize = row3.iter().map(|s| s.content.chars().count()).sum();
    let pct = sum.max_ctx_pct;
    let ctx_label = format!("  󰍛 {}% ", pct);
    let bar_label_w = ctx_label.chars().count();
    let bar_w = (inner.width as usize)
        .saturating_sub(left_w + bar_label_w)
        .min(20);
    if bar_w >= 4 {
        row3.push(Span::styled(ctx_label, Style::default().fg(ctx_color(pct))));
        row3.extend(ctx_bar(pct, bar_w));
    } else {
        row3.push(Span::styled(
            format!("  󰍛 {}%", pct),
            Style::default().fg(ctx_color(pct)),
        ));
    }
    lines.push(Line::from(row3));

    // Row 4: live tool / thinking line — only if we have one.
    if let Some((tool, hint)) = &sum.current_tool {
        let name = short_tool(tool);
        let max = inner.width.saturating_sub(4) as usize;
        let txt = match hint.as_deref().filter(|h| !h.is_empty()) {
            Some(h) => format!(
                "󰖷 {}: {}",
                name,
                models::first_line_truncated(h, max.saturating_sub(name.len() + 4))
            ),
            None => format!("󰖷 {}", name),
        };
        lines.push(Line::from(Span::styled(
            txt,
            Style::default().fg(Color::Rgb(180, 200, 160)),
        )));
    } else if sum.is_thinking {
        lines.push(Line::from(Span::styled(
            "󰟶 thinking",
            Style::default().fg(PURPLE),
        )));
    }

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

/// Borrowed view of [`crate::merge_lock::MergeLock`] plus the holder's
/// resolved title, supplied to Merging-column cards so a queued card can
/// distinguish a healthy queue from one stuck behind a wedged orchestrator.
pub(crate) struct MergeLockBanner<'a> {
    pub(super) task_id: &'a str,
    pub(super) title: Option<&'a str>,
    pub(super) acquired_at: i64,
    pub(super) phase: crate::merge_lock::MergePhase,
    pub(super) pr_id: Option<u32>,
}

/// Compact 3-line card for Review/Merging/Done tasks. Dim border, single-line
/// prompt, footer with age + summary preview + artifact/merge counts.
/// `col_idx` is one of 2 (Review), 3 (Merging), or 4 (Done).
/// `lock_holder` is the merge-lock holder context for this project, only
/// supplied for col_idx == 3. A Merging card whose id differs from the
/// holder is "queued" and renders with a muted border plus a waiting line
/// naming the holder and lock age, plus a tooltip showing the current phase
/// (e.g. "PR #N in /simplify (4m)") so the user can tell at a glance whether
/// the queue is healthy or stuck.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_task_card_collapsed(
    frame: &mut Frame,
    area: Rect,
    t: &crate::orchestrator::TaskState,
    selected: bool,
    col_idx: usize,
    sessions_by_tmux: &std::collections::HashMap<&str, &SessionInfo>,
    pr_summary: Option<&crate::projects_scan::PrCardSummary>,
    now_secs: u64,
    titling_in_flight: bool,
    lock_holder: Option<&MergeLockBanner<'_>>,
) {
    use crate::merge_lock::MergePhase;
    // Review (2) cyan, Merging (3) magenta, Done (4) green.
    let (accent, dim_text, icon) = match col_idx {
        2 => (Color::LightCyan, Color::Rgb(140, 175, 185), "󱋲"),
        3 => (Color::LightMagenta, Color::Rgb(180, 145, 195), ""),
        _ => (Color::LightGreen, Color::Rgb(140, 160, 145), "󰸞"),
    };
    let queued = col_idx == 3 && lock_holder.is_some_and(|h| h.task_id != t.task_id);
    let merging_self = col_idx == 3 && lock_holder.is_some_and(|h| h.task_id == t.task_id);
    // Review and Merging cards: brighter border so they stand out — they
    // need user attention or are actively mutating main. Done stays dim.
    // A queued Merging card uses a muted gray instead of the bright
    // magenta — approved, but waiting behind another merge.
    let (border_type, border_color) = if selected {
        (BorderType::Double, Color::White)
    } else if col_idx == 2 {
        (BorderType::Rounded, Color::Rgb(110, 170, 180))
    } else if queued {
        (BorderType::Rounded, TASK_META_DIM)
    } else if col_idx == 3 {
        (BorderType::Rounded, Color::Rgb(170, 130, 180))
    } else {
        (BorderType::Rounded, Color::Rgb(55, 60, 70))
    };
    let icon_accent = if queued {
        Color::Rgb(135, 135, 155)
    } else {
        accent
    };

    let short_id = crate::orchestrator::short_task_id(&t.task_id);
    let prompt_max = (area.width as usize).saturating_sub(8);
    let header_text = task_card_header_text(t, titling_in_flight, prompt_max);
    let mut title_spans = vec![
        Span::styled(format!(" {} ", icon), Style::default().fg(icon_accent)),
        Span::styled(
            header_text,
            Style::default()
                .fg(if selected { Color::White } else { dim_text })
                .add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
    ];
    if queued {
        title_spans.push(Span::styled(
            " queued ",
            Style::default()
                .fg(Color::Rgb(170, 170, 185))
                .bg(Color::Rgb(45, 45, 55)),
        ));
    } else if merging_self {
        title_spans.push(Span::styled(
            " merging ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(190, 145, 210))
                .add_modifier(Modifier::BOLD),
        ));
    }
    title_spans.push(Span::raw(" "));
    let title = Line::from(title_spans);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(border_color))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width < 4 {
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    if queued {
        // t.summary/t.note for an approved-but-waiting PR is already shown
        // on its Review card; on the Merging card the user wants who is
        // blocking and for how long instead.
        if let Some(h) = lock_holder {
            let body_max = inner.width.saturating_sub(4) as usize;
            let lock_age =
                format_duration_secs(now_secs.saturating_sub(h.acquired_at.max(0) as u64));
            let holder_short = crate::orchestrator::short_task_id(h.task_id);
            let age_suffix = format!(" · {}", lock_age);
            let holder_title = h.title.map(str::trim).filter(|s| !s.is_empty());
            let body = match holder_title {
                Some(title) => {
                    let prefix = format!("behind #{} · ", holder_short);
                    let reserved = prefix.chars().count() + age_suffix.chars().count();
                    let title_room = body_max.saturating_sub(reserved);
                    format!(
                        "{}{}{}",
                        prefix,
                        models::first_line_truncated(title, title_room),
                        age_suffix
                    )
                }
                None => format!("behind #{}{}", holder_short, age_suffix),
            };
            lines.push(Line::from(vec![
                Span::styled("󰔟 ", Style::default().fg(META_GRAY)),
                Span::styled(body, Style::default().fg(Color::Rgb(170, 170, 185))),
            ]));
        }
    } else {
        let summary_text = t
            .summary
            .as_deref()
            .or(t.note.as_deref())
            .map(|s| models::first_line_truncated(s, inner.width.saturating_sub(4) as usize))
            .unwrap_or_default();
        if !summary_text.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("󰍡 ", Style::default().fg(META_GRAY)),
                Span::styled(summary_text, Style::default().fg(Color::Rgb(160, 165, 175))),
            ]));
        }
    }
    if let Some(v) = t.shipped_version.as_deref().filter(|s| !s.is_empty()) {
        lines.push(Line::from(vec![
            Span::styled("󰓹 ", Style::default().fg(META_GRAY)),
            shipped_version_span(v),
        ]));
    }

    if queued {
        let banner = lock_holder.expect("queued implies holder");
        let age = models::relative_age(now_secs.saturating_sub(banner.acquired_at as u64));
        let phase_label = match banner.phase {
            MergePhase::Merging => "merging",
            MergePhase::Simplify => "/simplify",
            MergePhase::Bump => "/bump",
            MergePhase::FinalizePending => "finalizing",
        };
        let leader = match banner.pr_id {
            Some(n) => format!("PR #{} in {} ({})", n, phase_label, age),
            None => format!(
                "behind {} in {} ({})",
                crate::orchestrator::short_task_id(banner.task_id),
                phase_label,
                age
            ),
        };
        lines.push(Line::from(vec![
            Span::styled("⏳ ", Style::default().fg(META_GRAY)),
            Span::styled(leader, Style::default().fg(LABEL_GRAY)),
        ]));
    }

    let age = models::relative_age(now_secs.saturating_sub(t.updated_at as u64));
    let arts = t.artifacts.len();
    let merged = t.workers.iter().filter(|w| worker_was_merged(w, t)).count();
    let total_w = t.workers.len();
    let mut footer: Vec<Span<'static>> = vec![
        Span::styled(
            format!("#{}  ", short_id),
            Style::default().fg(TASK_META_DIM),
        ),
        Span::styled("󰔟 ", Style::default().fg(META_GRAY)),
        Span::styled(age, Style::default().fg(Color::Rgb(140, 145, 160))),
    ];
    if total_w > 0 {
        footer.push(Span::raw("   "));
        footer.push(Span::styled(
            format!("merges {}/{}", merged, total_w),
            Style::default().fg(Color::Rgb(140, 145, 160)),
        ));
    }
    if arts > 0 {
        footer.push(Span::raw("   "));
        footer.push(Span::styled(
            format!("󰉂 {}", arts),
            Style::default().fg(Color::Rgb(160, 145, 195)),
        ));
    }
    if let Some((done, total)) = todos_progress(t) {
        footer.push(Span::raw("   "));
        footer.push(Span::styled(
            format!("☑ {}/{}", done, total),
            Style::default().fg(Color::Rgb(140, 145, 160)),
        ));
    }
    let tool_uses = sum_tool_uses(t, sessions_by_tmux);
    if tool_uses > 0 {
        footer.push(Span::raw("   "));
        footer.push(Span::styled(
            format!("󰖷 {}", tool_uses),
            Style::default().fg(Color::Rgb(180, 200, 160)),
        ));
    }
    if let Some(pr) = pr_summary {
        footer.push(Span::raw("   "));
        footer.extend(pr_badge_spans(pr));
    }
    lines.push(Line::from(footer));

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

pub(crate) fn task_card_header_text(
    t: &crate::orchestrator::TaskState,
    titling_in_flight: bool,
    prompt_max: usize,
) -> String {
    match t.title.as_deref().filter(|s| !s.is_empty()) {
        Some(name) => models::first_line_truncated(name, prompt_max),
        None if titling_in_flight => models::first_line_truncated("✎ …", prompt_max),
        None => models::first_line_truncated(&t.prompt, prompt_max),
    }
}

/// Styled `vX.Y.Z` span for the shipped-version display. Idempotent on the
/// `v` prefix so `0.1` and `v0.1` both render as `v0.1`.
pub(crate) fn shipped_version_span(v: &str) -> Span<'static> {
    Span::styled(
        format!("v{}", v.trim_start_matches('v')),
        Style::default().fg(Color::Rgb(150, 200, 165)),
    )
}

#[cfg(test)]
mod kanban_card_tests {
    use crate::orchestrator::{TaskState, TaskStatus, TodoItem};
    use crate::ui::common::buffer_to_string;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn task_with_todos(status: TaskStatus, done: usize, total: usize) -> TaskState {
        let mut t = TaskState::new("p".into(), PathBuf::from("/tmp/p"), "prompt".into());
        t.status = status;
        t.title = Some("test card".into());
        t.todos = (0..total)
            .map(|i| TodoItem {
                text: format!("step {}", i),
                done: i < done,
            })
            .collect();
        t
    }

    #[test]
    fn collapsed_card_shows_todos_badge() {
        let t = task_with_todos(TaskStatus::Review, 2, 4);
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &t,
                    false,
                    2,
                    &sessions,
                    None,
                    1_000_000_000,
                    false,
                    None,
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(
            plain.contains("2/4"),
            "collapsed card should show 2/4 badge:\n{}",
            plain
        );
    }

    #[test]
    fn collapsed_card_omits_badge_when_no_todos() {
        let t = task_with_todos(TaskStatus::Review, 0, 0);
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &t,
                    false,
                    2,
                    &sessions,
                    None,
                    1_000_000_000,
                    false,
                    None,
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(!plain.contains("☑"), "no todos => no badge:\n{}", plain);
    }

    fn fake_session(tmux: &str, tool_uses: u64) -> super::SessionInfo {
        use crate::agent::AgentKind;
        use crate::models::SessionState;
        super::SessionInfo {
            agent_id: "claude".into(),
            agent_kind: AgentKind::Claude,
            pid: 0,
            session_id: "sid-x".into(),
            cwd: "/tmp".into(),
            project_name: "p".into(),
            started_at: 0,
            last_activity: None,
            state: SessionState::Processing,
            last_user_message: None,
            summary: None,
            title: None,
            titling: false,
            model: None,
            git_branch: None,
            version: None,
            jsonl_path: None,
            tmux_session: Some(tmux.into()),
            current_tool: None,
            is_thinking: false,
            context_tokens: None,
            tool_uses_count: tool_uses,
        }
    }

    /// Render a session card's title row (y=0) as one string of cell symbols.
    fn title_row(s: &super::SessionInfo, w: u16) -> String {
        let backend = TestBackend::new(w, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                crate::ui::sessions::render_card(f, f.area(), s, None, None, false, 1_000_000_000)
            })
            .expect("render");
        let buf = terminal.backend().buffer();
        (0..buf.area().width)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect()
    }

    // The state-indicator glyph is a Nerd Font icon that renders two columns
    // wide but measures as one, so the cell at its second-column position must
    // belong to the title (a space), never the border — otherwise a
    // border-only redraw overpaints the glyph's right half and it looks "cut".
    // Worst case is a no-title Claude card, whose title is just the bare icon.
    #[test]
    fn card_title_keeps_a_blank_after_the_state_icon() {
        use crate::models::SessionState;
        let mut s = fake_session("wk-1", 0);
        s.state = SessionState::WaitingForInput; // "󰂞"
        s.project_name = "cc-hub".into();

        for agent in ["claude", "codex"] {
            for (titling, title) in [
                (false, None),
                (true, None),
                (false, Some("Fix the parser".to_string())),
            ] {
                let mut s = s.clone();
                s.agent_id = agent.into();
                s.titling = titling;
                s.title = title.clone();
                for w in [42u16, 24, 16, 13] {
                    let backend = TestBackend::new(w, 6);
                    let mut terminal = Terminal::new(backend).expect("terminal");
                    terminal
                        .draw(|f| {
                            crate::ui::sessions::render_card(
                                f,
                                f.area(),
                                &s,
                                None,
                                None,
                                false,
                                1_000_000_000,
                            )
                        })
                        .expect("render");
                    let buf = terminal.backend().buffer();
                    let gx = (0..buf.area().width)
                        .find(|&x| buf[(x, 0)].symbol() == "󰂞")
                        .unwrap_or_else(|| {
                            panic!(
                                "icon missing (agent={agent}, w={w}):\n{}",
                                buffer_to_string(buf)
                            )
                        });
                    let right = buf[(gx + 1, 0)].symbol();
                    assert_eq!(
                        right, " ",
                        "icon needs a reserved blank to its right \
                         (agent={agent}, titling={titling}, title={title:?}, w={w}, got {right:?}):\n{}",
                        buffer_to_string(buf)
                    );
                }
            }
        }
    }

    // The title drops the redundant "[Claude]" badge and the project name
    // (the project is already the card group's header). Non-Claude agents
    // still get a badge so mixed fleets stay legible.
    #[test]
    fn card_title_omits_claude_badge_and_project_name() {
        let mut s = fake_session("wk-1", 0);
        s.project_name = "cc-hub".into();
        s.title = Some("Fix the parser".into());

        let claude = title_row(&s, 60);
        assert!(
            !claude.contains("Claude") && !claude.contains("cc-hub"),
            "Claude card title must not show the agent badge or project name:\n{claude}"
        );
        assert!(
            claude.contains("Fix the parser"),
            "Claude card title should still show the Haiku title:\n{claude}"
        );

        s.agent_id = "codex".into();
        let codex = title_row(&s, 60);
        assert!(
            codex.contains("codex"),
            "non-Claude agents should still show a badge:\n{codex}"
        );
    }

    fn task_with_worker(status: TaskStatus, worker_tmux: &str) -> TaskState {
        use crate::agent::AgentKind;
        use crate::orchestrator::Worker;
        let mut t = TaskState::new("p".into(), PathBuf::from("/tmp/p"), "prompt".into());
        t.status = status;
        t.title = Some("test card".into());
        t.workers.push(Worker {
            agent_id: "claude".into(),
            agent_kind: AgentKind::Claude,
            tmux_name: worker_tmux.into(),
            cwd: PathBuf::from("/tmp/p"),
            worktree: None,
            readonly: false,
            spawned_at: 0,
        });
        t
    }

    #[test]
    fn active_card_shows_tool_calls_badge() {
        let t = task_with_worker(TaskStatus::Running, "wk-1");
        let session = fake_session("wk-1", 7);
        let mut sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        sessions.insert("wk-1", &session);
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_active(
                    f,
                    f.area(),
                    &t,
                    false,
                    1,
                    &sessions,
                    None,
                    1_000_000_000,
                    false,
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(
            plain.contains("󰖷 7") || plain.contains(" 7"),
            "active card should show tool-uses badge with 7:\n{}",
            plain
        );
        assert!(
            plain.contains("󰖷"),
            "active card should show tool glyph:\n{}",
            plain
        );
    }

    #[test]
    fn active_card_omits_tool_calls_when_zero() {
        let t = task_with_todos(TaskStatus::Running, 0, 0);
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_active(
                    f,
                    f.area(),
                    &t,
                    false,
                    1,
                    &sessions,
                    None,
                    1_000_000_000,
                    false,
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(!plain.contains("󰖷"), "no tool uses => no badge:\n{}", plain);
    }

    #[test]
    fn collapsed_card_shows_tool_calls_badge() {
        let t = task_with_worker(TaskStatus::Review, "wk-2");
        let session = fake_session("wk-2", 5);
        let mut sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        sessions.insert("wk-2", &session);
        let backend = TestBackend::new(60, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &t,
                    false,
                    2,
                    &sessions,
                    None,
                    1_000_000_000,
                    false,
                    None,
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(
            plain.contains("󰖷") && plain.contains("5"),
            "collapsed card should show tool-uses badge with 5:\n{}",
            plain
        );
    }

    #[test]
    fn active_card_shows_todos_badge() {
        let t = task_with_todos(TaskStatus::Running, 2, 4);
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_active(
                    f,
                    f.area(),
                    &t,
                    false,
                    1,
                    &sessions,
                    None,
                    1_000_000_000,
                    false,
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(
            plain.contains("2/4"),
            "active card should show 2/4 badge:\n{}",
            plain
        );
    }

    fn merging_task(task_id: &str) -> TaskState {
        let mut t = TaskState::new("p".into(), PathBuf::from("/tmp/p"), "prompt".into());
        t.task_id = task_id.to_string();
        t.status = TaskStatus::Merging;
        t.title = Some("waiting card".into());
        t
    }

    fn pr_summary(
        id: u32,
        state: crate::pr::ReviewState,
        comments: u16,
    ) -> crate::projects_scan::PrCardSummary {
        crate::projects_scan::PrCardSummary {
            id,
            review_state: state,
            comments,
        }
    }

    fn render_active(
        t: &TaskState,
        pr: Option<&crate::projects_scan::PrCardSummary>,
        col_idx: usize,
    ) -> String {
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(70, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_active(
                    f,
                    f.area(),
                    t,
                    false,
                    col_idx,
                    &sessions,
                    pr,
                    1_000_000_000,
                    false,
                )
            })
            .expect("render");
        buffer_to_string(terminal.backend().buffer())
    }

    fn render_collapsed(
        t: &TaskState,
        pr: Option<&crate::projects_scan::PrCardSummary>,
        col_idx: usize,
    ) -> String {
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(70, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    t,
                    false,
                    col_idx,
                    &sessions,
                    pr,
                    1_000_000_000,
                    false,
                    None,
                )
            })
            .expect("render");
        buffer_to_string(terminal.backend().buffer())
    }

    #[test]
    fn collapsed_card_shows_holder_context_when_queued() {
        let t = merging_task("t-self-000111");
        let now: u64 = 1_700_000_000;
        let banner = super::MergeLockBanner {
            task_id: "t-blocker-123456",
            title: Some("fix flaky tests"),
            acquired_at: (now - 180) as i64,
            phase: crate::merge_lock::MergePhase::Merging,
            pr_id: None,
        };
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(60, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &t,
                    false,
                    3,
                    &sessions,
                    None,
                    now,
                    false,
                    Some(&banner),
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(
            plain.contains("behind"),
            "queued card should name the holder:\n{}",
            plain
        );
        assert!(
            plain.contains("123456"),
            "queued card should show holder short id:\n{}",
            plain
        );
        assert!(
            plain.contains("3m"),
            "queued card should show 3m lock age:\n{}",
            plain
        );
        assert!(
            plain.contains("queued"),
            "queued card should keep the queued pill:\n{}",
            plain
        );
    }

    #[test]
    fn queued_collapsed_card_shows_holder_phase_and_pr() {
        use crate::merge_lock::MergePhase;
        let t = merging_task("t-self-000222");
        let now: u64 = 1_700_000_000;
        let banner = super::MergeLockBanner {
            task_id: "t-other-000333",
            title: Some("blocking work"),
            acquired_at: (now - 240) as i64,
            phase: MergePhase::Simplify,
            pr_id: Some(7),
        };
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(70, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &t,
                    false,
                    3,
                    &sessions,
                    None,
                    now,
                    false,
                    Some(&banner),
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(
            plain.contains("PR #7"),
            "queued card should show holder PR id:\n{}",
            plain
        );
        assert!(
            plain.contains("/simplify"),
            "queued card should show holder phase:\n{}",
            plain
        );
        assert!(
            plain.contains("4m"),
            "queued card should show holder age:\n{}",
            plain
        );
    }

    #[test]
    fn collapsed_card_shows_merging_pill_when_self_is_holder() {
        let t = merging_task("t-self-654321");
        let now: u64 = 1_700_000_000;
        let banner = super::MergeLockBanner {
            task_id: "t-self-654321",
            title: Some("self merge"),
            acquired_at: (now - 5) as i64,
            phase: crate::merge_lock::MergePhase::Merging,
            pr_id: None,
        };
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(60, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &t,
                    false,
                    3,
                    &sessions,
                    None,
                    now,
                    false,
                    Some(&banner),
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(
            plain.contains("merging"),
            "self-holder card should show the merging pill:\n{}",
            plain
        );
        assert!(
            !plain.contains("behind"),
            "self-holder card must not render a waiting line:\n{}",
            plain
        );
    }

    /// Visual-proof helper — not part of the regression suite. Dumps the
    /// rendered card buffers for three Merging states (queued, holder, no
    /// lock) to /tmp so a screenshot artifact can be attached to the PR.
    /// Run with: `cargo test -p cc-hub-lib dump_merging_states -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn dump_merging_states() {
        let now: u64 = 1_700_000_000;
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();

        let queued = merging_task("t-self-000111");
        let banner_blocking = super::MergeLockBanner {
            task_id: "t-blocker-123456",
            title: Some("fix flaky tests"),
            acquired_at: (now - 180) as i64,
            phase: crate::merge_lock::MergePhase::Merging,
            pr_id: None,
        };
        let backend = TestBackend::new(60, 4);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &queued,
                    false,
                    3,
                    &sessions,
                    None,
                    now,
                    false,
                    Some(&banner_blocking),
                )
            })
            .expect("render");
        let queued_render = buffer_to_string(terminal.backend().buffer());

        let holder = merging_task("t-blocker-123456");
        let banner_self = super::MergeLockBanner {
            task_id: "t-blocker-123456",
            title: Some("fix flaky tests"),
            acquired_at: (now - 12) as i64,
            phase: crate::merge_lock::MergePhase::Merging,
            pr_id: None,
        };
        let backend = TestBackend::new(60, 4);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &holder,
                    false,
                    3,
                    &sessions,
                    None,
                    now,
                    false,
                    Some(&banner_self),
                )
            })
            .expect("render");
        let holder_render = buffer_to_string(terminal.backend().buffer());

        let alone = merging_task("t-self-000111");
        let backend = TestBackend::new(60, 4);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &alone,
                    false,
                    3,
                    &sessions,
                    None,
                    now,
                    false,
                    None,
                )
            })
            .expect("render");
        let alone_render = buffer_to_string(terminal.backend().buffer());

        let dump = format!(
            "=== Merging column · QUEUED (lock held by another task) ===\n\
             {queued}\n\
             === Merging column · ACTIVE (this card holds the lock) ===\n\
             {holder}\n\
             === Merging column · NO LOCK (no holder recorded) ===\n\
             {alone}\n",
            queued = queued_render,
            holder = holder_render,
            alone = alone_render,
        );
        std::fs::write("/tmp/cchub-merging-states.txt", &dump).expect("write dump");
        eprintln!("{}", dump);
    }

    #[test]
    fn collapsed_card_no_holder_context_when_no_lock() {
        let t = merging_task("t-only-999999");
        let sessions: HashMap<&str, &super::SessionInfo> = HashMap::new();
        let backend = TestBackend::new(60, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                super::render_task_card_collapsed(
                    f,
                    f.area(),
                    &t,
                    false,
                    3,
                    &sessions,
                    None,
                    1_700_000_000,
                    false,
                    None,
                )
            })
            .expect("render");
        let plain = buffer_to_string(terminal.backend().buffer());
        assert!(
            !plain.contains("behind"),
            "no holder => no waiting line:\n{}",
            plain
        );
        assert!(
            !plain.contains("queued"),
            "no holder => no queued pill:\n{}",
            plain
        );
    }

    #[test]
    fn active_card_shows_pr_changes_requested_badge() {
        let t = task_with_todos(TaskStatus::Running, 0, 0);
        let pr = pr_summary(42, crate::pr::ReviewState::ChangesRequested, 3);
        let plain = render_active(&t, Some(&pr), 1);
        assert!(
            plain.contains("PR #42") && plain.contains("changes requested"),
            "changes_requested badge missing:\n{}",
            plain
        );
        assert!(plain.contains("3"), "comment count missing:\n{}", plain);
    }

    #[test]
    fn collapsed_card_shows_pr_open_with_comment_count() {
        let t = task_with_todos(TaskStatus::Review, 0, 0);
        let pr = pr_summary(7, crate::pr::ReviewState::Open, 2);
        let plain = render_collapsed(&t, Some(&pr), 2);
        assert!(plain.contains("PR #7"), "PR id missing:\n{}", plain);
        assert!(
            plain.contains("2"),
            "comment count missing on open PR:\n{}",
            plain
        );
    }

    #[test]
    fn collapsed_card_open_without_comments_shows_open_label() {
        let t = task_with_todos(TaskStatus::Review, 0, 0);
        let pr = pr_summary(9, crate::pr::ReviewState::Open, 0);
        let plain = render_collapsed(&t, Some(&pr), 2);
        assert!(plain.contains("PR #9"), "PR id missing:\n{}", plain);
        assert!(
            plain.contains("open"),
            "open label missing when no comments:\n{}",
            plain
        );
    }

    #[test]
    fn active_card_omits_pr_badge_when_no_pr() {
        let t = task_with_todos(TaskStatus::Running, 0, 0);
        let plain = render_active(&t, None, 1);
        assert!(!plain.contains("PR #"), "no PR ⇒ no badge:\n{}", plain);
    }
}
