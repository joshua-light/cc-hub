use super::wrap_text;
use crate::models::{self, SessionInfo, SessionState};
use crate::tasks::activity::Errand;
use crate::tasks::store::{TaskState, TaskStatus};
use crate::ui::common::priority_color;
use crate::ui::palette::{ACCENT_BLUE, DIM_TEXT, DOT_IDLE, KIND_TEAL, META_GRAY, TAG_SLATE};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;
use std::collections::HashMap;

#[allow(clippy::too_many_arguments)]
pub(super) fn render_task_card(
    frame: &mut Frame,
    area: Rect,
    t: &TaskState,
    selected: bool,
    status: TaskStatus,
    accent: Color,
    sessions_by_tmux: &HashMap<&str, &SessionInfo>,
    now_secs: u64,
) {
    let (border_type, border_style) = if selected {
        (BorderType::Double, Style::default().fg(accent))
    } else {
        (
            BorderType::Rounded,
            Style::default().fg(Color::Rgb(60, 60, 80)),
        )
    };
    // Priority badge rides the top-right of the border (`P1`–`P4`): black
    // text on a color-filled chip, prominent without stealing a text row. It
    // keeps its own colours even when the selected border turns the accent.
    let priority_badge = Line::from(Span::styled(
        format!(" {} ", t.priority.label()),
        Style::default()
            .fg(Color::Black)
            .bg(priority_color(t.priority))
            .add_modifier(Modifier::BOLD),
    ))
    .alignment(Alignment::Right);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style)
        .title(priority_badge);
    // The kind chip and the tags ride the top-left of the border, mirroring
    // the priority chip on the right. Budget the width against what the
    // priority chip leaves (corners + chip + a one-column gap) so the badges
    // never collide; the kind takes its room first (it decides where the card
    // is worked, the tags only describe it) and tag overflow folds into a
    // `+N` marker.
    let prio_w = t.priority.label().chars().count() + 2;
    let mut budget = (area.width as usize).saturating_sub(2 + prio_w + 1);
    let mut badges: Vec<Span> = Vec::new();
    if let Some(chip) = kind_chip_text(t.kind.as_deref(), budget) {
        budget -= chip.chars().count();
        badges.push(Span::styled(
            chip,
            Style::default()
                .fg(Color::Black)
                .bg(KIND_TEAL)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(text) = tags_title_text(&t.tags, budget) {
        badges.push(Span::styled(text, Style::default().fg(TAG_SLATE)));
    }
    if !badges.is_empty() {
        block = block.title(Line::from(badges).alignment(Alignment::Left));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let done = status == TaskStatus::Done;
    let text_style = if done {
        Style::default().fg(DIM_TEXT).add_modifier(Modifier::ITALIC)
    } else if selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Rgb(200, 200, 210))
    };

    let stats_rows = usize::from(done);
    let text_rows = (inner.height as usize)
        .saturating_sub(1 + stats_rows)
        .max(1);
    let mut lines: Vec<Line> = wrap_text(&t.prompt, inner.width as usize)
        .into_iter()
        .take(text_rows)
        .map(|seg| Line::from(Span::styled(seg, text_style)))
        .collect();

    // Pin the meta row to the card's bottom edge so short and wrapped texts
    // produce the same silhouette.
    while lines.len() < text_rows {
        lines.push(Line::raw(""));
    }
    if done {
        let label = t
            .stats
            .as_ref()
            .map(|stats| stats.compact())
            .unwrap_or_else(|| "Stats unavailable".into());
        lines.push(Line::from(Span::styled(
            label,
            Style::default().fg(META_GRAY),
        )));
    }
    lines.push(meta_line(t, status, sessions_by_tmux, now_secs));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// One row of facts the column/border can't already say: how old the task
/// is, and — once an agent is bound — who runs it, where, and what state the
/// session is in right now.
fn meta_line(
    t: &TaskState,
    status: TaskStatus,
    sessions_by_tmux: &HashMap<&str, &SessionInfo>,
    now_secs: u64,
) -> Line<'static> {
    let age_style = Style::default().fg(META_GRAY);
    // Attachment chip: rendered at the end of either branch so a card with
    // reference docs is recognizable from the board.
    let clip = (!t.artifacts.is_empty())
        .then(|| Span::styled(format!("  📎{}", t.artifacts.len()), age_style));
    if status == TaskStatus::Done {
        let when = t.done_at.unwrap_or(t.created_at);
        let mut spans = vec![Span::styled(
            format!(
                "✓ {}",
                models::relative_age(now_secs.saturating_sub(when.max(0) as u64))
            ),
            age_style,
        )];
        // A done task that ran through an agent keeps its transcript — `f`
        // still opens/resumes it — so say who ran it and where. Marking done
        // closes the live session, but one can outlive that (close failed,
        // board edited by hand), so a survivor still gets the brighter dot.
        if t.tmux.is_some() || t.session_id.is_some() {
            let agent = t.agent_id.as_deref().unwrap_or("agent");
            spans.push(Span::styled(format!("  󰚩 {}", agent), age_style));
            if let Some(dir) = t.cwd.as_deref().map(dir_basename) {
                spans.push(Span::styled(format!(" · {}", dir), age_style));
            }
            if t.tmux
                .as_deref()
                .is_some_and(|n| sessions_by_tmux.contains_key(n))
            {
                spans.push(Span::styled(
                    "  ● live",
                    Style::default().fg(Color::LightGreen),
                ));
            }
        }
        spans.extend(clip);
        return Line::from(spans);
    }

    let mut spans: Vec<Span> = Vec::new();
    if let Some(tmux) = t.tmux.as_deref() {
        let (glyph, label, color) = match sessions_by_tmux.get(tmux) {
            Some(s) => match s.state {
                SessionState::Processing => ("⟳", "working", Color::LightYellow),
                SessionState::WaitingForInput | SessionState::Question => {
                    ("󰂞", "needs input", Color::Yellow)
                }
                // An idle planning agent has finished its turn — the plan is
                // sitting in the transcript waiting for a verdict. Keyed off
                // the card's own status so it still reads "plan ready" when a
                // Planning card is folded into a hidden-Planning In Progress
                // column.
                SessionState::Idle if t.status == TaskStatus::Planning => {
                    ("●", "plan ready", Color::LightGreen)
                }
                // An idle implementation agent may be stalled; explicit
                // activity records below identify completed work.
                SessionState::Idle if t.status == TaskStatus::Running => {
                    ("●", "idle — check progress", Color::LightCyan)
                }
                SessionState::Idle => ("●", "idle", Color::LightGreen),
                // App-synthesized spawn placeholder; task-linked sessions come
                // from the scanner so this arm is exhaustiveness-only.
                SessionState::Starting => ("◌", "starting…", DOT_IDLE),
                SessionState::Inactive => ("○", "inactive", DOT_IDLE),
            },
            // Spawned but not scanned yet, or the tmux died. With a resolved
            // session id `f` resumes; before resolution it can only hint.
            None if t.session_id.is_some() => ("○", "gone — f resumes", DOT_IDLE),
            None => ("○", "starting…", DOT_IDLE),
        };
        // What the card itself records outranks what the session looks like:
        // an open question is not idle (it reads as `starting…` forever
        // otherwise), and a session that has opened a PR is not asking for
        // input even when its pane sits at a prompt — the distinction the
        // Review column is drawn along, carried onto the card in the
        // column's own glyph and hue.
        let activity = crate::tasks::activity::label(&t.task_id);
        let (glyph, color) = match activity.as_ref().map(|item| item.errand) {
            Some(Errand::Answer) => (glyph, Color::Yellow),
            Some(Errand::Review) => crate::ui::common::task_status_meta(TaskStatus::Review),
            _ => (glyph, color),
        };
        spans.push(Span::styled(
            format!(
                "{} {}",
                glyph,
                activity.as_ref().map_or(label, |item| item.text.as_str())
            ),
            Style::default().fg(color),
        ));
        if let Some(dir) = t.cwd.as_deref().map(dir_basename) {
            spans.push(Span::styled("  ", age_style));
            spans.push(Span::styled(dir, Style::default().fg(ACCENT_BLUE)));
        }
        spans.push(Span::styled("  ", age_style));
    }
    spans.push(Span::styled(
        models::relative_age(now_secs.saturating_sub(t.created_at.max(0) as u64)),
        age_style,
    ));
    spans.extend(clip);
    Line::from(spans)
}

/// The left-border kind chip (` tps `) when the card has a kind and it fits
/// in `budget` columns. A kind is never abbreviated — a half-written `ai-plu`
/// would read as a different kind — so a chip that doesn't fit is dropped and
/// the Task Info popup remains where the full word is.
fn kind_chip_text(kind: Option<&str>, budget: usize) -> Option<String> {
    let chip = format!(" {} ", kind?);
    (chip.chars().count() <= budget).then_some(chip)
}

/// Build the left-border tag badge text (`#a #b`) that fits in `budget`
/// columns. Takes whole tags greedily; if any don't fit, the leftover count
/// folds into a trailing `+N` (reserving room for it, single-digit since the
/// tag set is capped). Returns `None` when there are no tags or no room.
fn tags_title_text(tags: &[String], budget: usize) -> Option<String> {
    if tags.is_empty() || budget < 3 {
        return None;
    }
    // Each piece carries its own leading space so the run insets from the
    // corner like the priority chip's ` P1 `.
    let pieces: Vec<String> = tags.iter().map(|t| format!(" #{}", t)).collect();
    let mut used = 0usize;
    let mut shown = 0usize;
    for (i, piece) in pieces.iter().enumerate() {
        let more_after = i + 1 < pieces.len();
        // Keep room for a ` +N` marker (≤3 cols) whenever tags would remain.
        let reserve = if more_after { 3 } else { 0 };
        if used + piece.chars().count() + reserve > budget {
            break;
        }
        used += piece.chars().count();
        shown += 1;
    }
    let hidden = tags.len() - shown;
    if shown == 0 {
        // Not even one tag fits beside the priority chip — show just the count.
        return Some(format!(" +{}", tags.len()));
    }
    let mut out: String = pieces[..shown].concat();
    if hidden > 0 {
        out.push_str(&format!(" +{}", hidden));
    }
    Some(out)
}

fn dir_basename(cwd: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::store::{TaskPriority, TaskStatus};
    use crate::ui::common::buffer_to_string;
    use crate::ui::palette::BACKLOG_BLUE;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn card(priority: TaskPriority) -> TaskState {
        let mut t = TaskState::new("fix the parser".into());
        t.task_id = "tk-1".into();
        t.priority = priority;
        t.created_at = 1;
        t
    }

    /// Render a To-Do card and return its painted buffer.
    fn render(priority: TaskPriority) -> ratatui::buffer::Buffer {
        render_card(&card(priority))
    }

    /// Render an arbitrary card (wide enough for badges) and return its buffer.
    fn render_card(t: &TaskState) -> ratatui::buffer::Buffer {
        let sessions = HashMap::new();
        let backend = TestBackend::new(28, 5);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                render_task_card(
                    f,
                    f.area(),
                    t,
                    false,
                    TaskStatus::Backlog,
                    BACKLOG_BLUE,
                    &sessions,
                    1_000,
                );
            })
            .expect("render");
        terminal.backend().buffer().clone()
    }

    #[test]
    fn card_shows_priority_badge() {
        let buf = render(TaskPriority::P2);
        assert!(
            buffer_to_string(&buf).contains("P2"),
            "card should show its priority badge:\n{}",
            buffer_to_string(&buf)
        );
    }

    #[test]
    fn card_shows_tags_alongside_priority() {
        let mut t = card(TaskPriority::P1);
        t.tags = vec!["bug".into(), "api".into()];
        let painted = buffer_to_string(&render_card(&t));
        assert!(
            painted.contains("#bug") && painted.contains("#api"),
            "card should show its tag badges:\n{}",
            painted
        );
        // The priority chip must survive alongside the tags.
        assert!(
            painted.contains("P1"),
            "tags must not displace the priority badge:\n{}",
            painted
        );
    }

    #[test]
    fn card_shows_attachment_chip() {
        let mut t = card(TaskPriority::P3);
        t.artifacts.push(crate::tasks::store::Artifact {
            kind: "file".into(),
            path: "/tmp/store/1-doc.md".into(),
            original: "/tmp/doc.md".into(),
            caption: None,
            added_at: 1,
        });
        let painted = buffer_to_string(&render_card(&t));
        // The emoji is double-width, so the test backend inserts a placeholder
        // space cell after it — compare with spaces squeezed out.
        assert!(
            painted.replace(' ', "").contains("📎1"),
            "card should show the attachment chip:\n{}",
            painted
        );
        // A card without attachments shows no chip.
        let bare = buffer_to_string(&render(TaskPriority::P3));
        assert!(!bare.contains("📎"), "no chip expected:\n{}", bare);
    }

    #[test]
    fn card_shows_kind_chip_before_its_tags() {
        let mut t = card(TaskPriority::P1);
        t.kind = Some("hub".into());
        t.tags = vec!["bug".into()];
        let painted = buffer_to_string(&render_card(&t));
        let top = painted.lines().next().expect("border row");
        assert!(
            top.find("hub")
                .is_some_and(|kind| top.find("#bug").is_some_and(|tag| kind < tag)),
            "kind chip should take the left corner ahead of the tags:\n{}",
            painted
        );
        assert!(
            top.contains("P1"),
            "the kind must not displace the priority badge:\n{}",
            painted
        );
        // A card with no kind shows no chip.
        assert!(
            !buffer_to_string(&render(TaskPriority::P1)).contains("hub"),
            "router-classified cards carry no chip"
        );
    }

    #[test]
    fn kind_chip_is_dropped_rather_than_abbreviated() {
        assert_eq!(
            kind_chip_text(Some("ai-plugin"), 11).as_deref(),
            Some(" ai-plugin ")
        );
        assert_eq!(kind_chip_text(Some("ai-plugin"), 10), None);
        assert_eq!(kind_chip_text(None, 40), None);
    }

    #[test]
    fn tags_title_overflows_to_count_marker() {
        // Plenty of tags but a tiny budget folds the leftovers into `+N`.
        let tags: Vec<String> = vec!["alpha".into(), "bravo".into(), "charlie".into()];
        let text = tags_title_text(&tags, 10).expect("some tags fit");
        assert!(text.contains('+'), "expected overflow marker in {:?}", text);
        // No tags at all, or no room, yields nothing.
        assert_eq!(tags_title_text(&[], 20), None);
        assert_eq!(tags_title_text(&tags, 2), None);
    }

    #[test]
    fn completed_card_renders_usage_and_cost() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut task = card(TaskPriority::P3);
        task.status = TaskStatus::Done;
        task.stats = Some(crate::tasks::stats::TaskStats {
            input_tokens: 123_000,
            cost_nano_usd: Some(250_000_000),
            estimated: true,
            ..Default::default()
        });
        let mut terminal = Terminal::new(TestBackend::new(44, 5)).unwrap();
        terminal
            .draw(|frame| {
                render_task_card(
                    frame,
                    frame.area(),
                    &task,
                    false,
                    TaskStatus::Done,
                    Color::Green,
                    &HashMap::new(),
                    1_000,
                )
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("123.0K tokens · ~$0.2500"), "{text}");
    }

    fn idle_session(tmux: &str) -> SessionInfo {
        SessionInfo {
            agent_id: "claude".into(),
            agent_kind: crate::agent::AgentKind::Claude,
            pid: 1,
            session_id: tmux.into(),
            cwd: "/tmp".into(),
            project_name: "tmp".into(),
            started_at: 0,
            last_activity: None,
            state: SessionState::Idle,
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
            tool_uses_count: 0,
        }
    }

    #[test]
    fn idle_agent_badge_depends_on_card_status() {
        // The same idle session reads differently by phase: a Planning card
        // has a plan waiting, an In Progress card an implementation.
        let session = idle_session("mux-1");
        let sessions: HashMap<&str, &SessionInfo> = [("mux-1", &session)].into();
        let mut t = card(TaskPriority::P3);
        t.tmux = Some("mux-1".into());

        t.status = TaskStatus::Planning;
        let line = meta_line(&t, t.status, &sessions, 1_000).to_string();
        assert!(line.contains("plan ready"), "line: {line}");

        t.status = TaskStatus::Running;
        let line = meta_line(&t, t.status, &sessions, 1_000).to_string();
        assert!(line.contains("idle — check progress"), "line: {line}");
    }

    /// The reading the Review column exists for: a session that opened a PR
    /// and went back to its prompt looks exactly like one holding a question
    /// for you, so the card's own `PR:` note — not the pane — has the say.
    #[cfg(unix)]
    #[test]
    fn a_pr_note_reads_as_a_review_not_as_a_question() {
        crate::test_util::with_temp_home(|| {
            let mut board = crate::tasks::PersonalBoard::load();
            let id = board.add("ship the linter").unwrap().unwrap();
            board.assign(&id, "/tmp", "claude", "mux-pr").unwrap();
            crate::ops::task::task_artifact_add_text(&id, "PR: sample-project#42", "cli").unwrap();
            let t = crate::tasks::store::read_task_state(&id).unwrap();
            assert_eq!(t.status, TaskStatus::Review, "the note moved the card");

            let mut session = idle_session("mux-pr");
            session.state = SessionState::WaitingForInput;
            let sessions: HashMap<&str, &SessionInfo> = [("mux-pr", &session)].into();

            let line = meta_line(&t, t.status, &sessions, 1_000).to_string();
            assert!(line.contains("PR ready: sample-project#42"), "line: {line}");
            assert!(!line.contains("needs input"), "line: {line}");
        });
    }

    #[test]
    fn badge_is_color_coded_per_priority() {
        for (p, want) in [
            (TaskPriority::P1, Color::LightRed),
            (TaskPriority::P2, Color::LightYellow),
            (TaskPriority::P3, Color::LightGreen),
            (TaskPriority::P4, Color::LightBlue),
        ] {
            let buf = render(p);
            let area = *buf.area();
            // Find the badge's "P" cell: the hue fills the background, with
            // black text on top.
            let (fg, bg) = (0..area.width)
                .flat_map(|x| (0..area.height).map(move |y| (x, y)))
                .find(|&(x, y)| buf[(x, y)].symbol() == "P")
                .map(|(x, y)| (buf[(x, y)].fg, buf[(x, y)].bg))
                .expect("badge P cell present");
            assert_eq!(bg, want, "{} badge fill", p.label());
            assert_eq!(fg, Color::Black, "{} badge text", p.label());
        }
    }
}
