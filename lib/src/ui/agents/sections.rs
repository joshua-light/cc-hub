use super::detail::{render_empty, render_pane, render_rows, split_body};
use super::text::{clock, event_label, one_line, tilde};
use crate::app::Detail;
use crate::harness::settings::{Setting, SETTINGS};
use crate::harness::{AgentSnapshot, LogLine, Note, Run, TickRecord};
use crate::ui::common::{age, fmt_cost, format_tokens, pad, pad_left, spinner_frame};
use crate::ui::palette::{ACCENT_BLUE, DIM_TEXT, FAINT_TEXT, LABEL_GRAY};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

// ---- runs -----------------------------------------------------------------

const RUN_N_W: usize = 6;
const RUN_AGE_W: usize = 9;
const RUN_EVENT_W: usize = 14;
const RUN_DUR_W: usize = 7;
const RUN_COST_W: usize = 7;

fn run_row(run: &Run, now: i64) -> Line<'static> {
    let dim = Style::default().fg(DIM_TEXT);
    match run {
        Run::InFlight { n, ticking } => Line::from(vec![
            Span::styled(
                format!("{} ", spinner_frame(crate::ui::now_ms())),
                Style::default().fg(Color::Green),
            ),
            Span::styled(
                pad(&format!("#{}", n), RUN_N_W),
                Style::default().fg(LABEL_GRAY),
            ),
            Span::styled(
                pad(&format!("{} ago", age(now, ticking.since)), RUN_AGE_W),
                dim,
            ),
            Span::styled(
                pad(&event_label(ticking.event.as_deref()), RUN_EVENT_W),
                Style::default().fg(FAINT_TEXT),
            ),
            Span::raw(" ".repeat(RUN_DUR_W + RUN_COST_W + 2)),
            Span::styled("running…", Style::default().fg(Color::Green)),
        ]),
        Run::Done { n, rec } => {
            let (mark, color) = if rec.ok {
                ("✓", Color::Green)
            } else {
                ("✗", Color::Red)
            };
            let outcome = if rec.ok {
                Span::styled(one_line(&rec.result), Style::default().fg(FAINT_TEXT))
            } else {
                Span::styled(
                    rec.subtype.clone().unwrap_or_else(|| "failed".into()),
                    Style::default().fg(Color::Red),
                )
            };
            Line::from(vec![
                Span::styled(format!("{} ", mark), Style::default().fg(color)),
                Span::styled(
                    pad(&format!("#{}", n), RUN_N_W),
                    Style::default().fg(LABEL_GRAY),
                ),
                Span::styled(pad(&format!("{} ago", age(now, rec.at)), RUN_AGE_W), dim),
                Span::styled(
                    pad(&event_label(rec.event.as_deref()), RUN_EVENT_W),
                    Style::default().fg(FAINT_TEXT),
                ),
                Span::styled(
                    pad_left(
                        &crate::ui::common::format_duration_secs(rec.duration_s),
                        RUN_DUR_W,
                    ),
                    dim,
                ),
                Span::styled(pad_left(&fmt_cost(rec.cost_usd), RUN_COST_W), dim),
                Span::raw("  "),
                outcome,
            ])
        }
    }
}

fn run_pane(run: &Run, now: i64) -> Vec<Line<'static>> {
    let label = Style::default().fg(LABEL_GRAY);
    let dim = Style::default().fg(DIM_TEXT);
    let mut lines = Vec::new();
    match run {
        Run::InFlight { n, ticking } => {
            lines.push(Line::from(vec![
                Span::styled(format!("#{} ", n), label),
                Span::styled(
                    format!(
                        "running since {} · {}",
                        clock(ticking.since, now),
                        event_label(ticking.event.as_deref())
                    ),
                    Style::default().fg(Color::Green),
                ),
            ]));
            lines.push(Line::from(Span::styled("f follows it live.", dim)));
        }
        Run::Done { n, rec } => {
            lines.push(Line::from(vec![
                Span::styled(format!("#{} ", n), label),
                Span::styled(run_facts(rec, now), dim),
            ]));
            let style = Style::default().fg(if rec.ok { FAINT_TEXT } else { Color::Red });
            if !rec.result.is_empty() {
                lines.push(Line::from(Span::styled(rec.result.clone(), style)));
            }
            if let Some(d) = &rec.detail {
                lines.push(Line::from(Span::styled(format!("stderr: {}", d), dim)));
            }
            if let Some(ev) = &rec.event {
                lines.push(Line::from(Span::styled(format!("event {}", ev), dim)));
            }
        }
    }
    lines
}

fn run_facts(rec: &TickRecord, now: i64) -> String {
    let mut facts = vec![
        clock(rec.at, now),
        crate::ui::common::format_duration_secs(rec.duration_s),
        format!("{} turns", rec.turns),
        fmt_cost(rec.cost_usd),
    ];
    if rec.context_end > 0 {
        facts.push(format!(
            "context {} → {}",
            format_tokens(rec.context_start),
            format_tokens(rec.context_end)
        ));
    }
    if rec.compactions > 0 {
        facts.push(format!("{} compactions", rec.compactions));
    }
    if !rec.ok {
        facts.insert(0, rec.subtype.clone().unwrap_or_else(|| "failed".into()));
    }
    facts.join(" · ")
}

pub(super) fn render_runs(
    frame: &mut Frame,
    body: Rect,
    agent: &AgentSnapshot,
    d: &Detail,
    scroll: &mut usize,
    now: i64,
) {
    let runs = agent.runs();
    if runs.is_empty() {
        render_empty(frame, body, "No runs yet. p queues one now.");
        return;
    }
    let (list, pane) = split_body(body, runs.len());
    let rows = runs.iter().map(|r| run_row(r, now)).collect();
    render_rows(frame, list, rows, d.run, scroll);
    if let Some(run) = runs.get(d.run.min(runs.len() - 1)) {
        render_pane(frame, pane, run_pane(run, now));
    }
}

// ---- artifacts ------------------------------------------------------------

fn note_row(note: &Note, now: i64) -> Line<'static> {
    let (mark, color) = if note.level == "warn" {
        ("●", Color::Yellow)
    } else {
        ("·", DIM_TEXT)
    };
    let mut spans = vec![
        Span::styled(format!("{} ", mark), Style::default().fg(color)),
        Span::styled(
            pad(&format!("{} ago", age(now, note.at)), RUN_AGE_W),
            Style::default().fg(DIM_TEXT),
        ),
    ];
    if note.r#ref.is_some() {
        spans.push(Span::styled("󰌷 ", Style::default().fg(ACCENT_BLUE)));
    } else {
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(
        one_line(&note.text),
        Style::default().fg(FAINT_TEXT),
    ));
    Line::from(spans)
}

pub(super) fn render_artifacts(
    frame: &mut Frame,
    body: Rect,
    agent: &AgentSnapshot,
    d: &Detail,
    scroll: &mut usize,
    now: i64,
) {
    if agent.notes.is_empty() {
        render_empty(
            frame,
            body,
            "Nothing reported yet. An agent reports with `cc-hub agent note --text … --ref <url or path>`.",
        );
        return;
    }
    let (list, pane) = split_body(body, agent.notes.len());
    let rows = agent.notes.iter().map(|n| note_row(n, now)).collect();
    render_rows(frame, list, rows, d.artifact, scroll);
    let note = &agent.notes[d.artifact.min(agent.notes.len() - 1)];
    let mut lines = vec![
        Line::from(Span::styled(
            format!("run #{} · {}", note.tick, clock(note.at, now)),
            Style::default().fg(DIM_TEXT),
        )),
        Line::from(Span::styled(
            note.text.clone(),
            Style::default().fg(Color::White),
        )),
    ];
    if let Some(r) = &note.r#ref {
        lines.push(Line::from(vec![
            Span::styled("󰌷 ", Style::default().fg(ACCENT_BLUE)),
            Span::styled(r.clone(), Style::default().fg(ACCENT_BLUE)),
            Span::styled("   f opens it", Style::default().fg(DIM_TEXT)),
        ]));
    }
    render_pane(frame, pane, lines);
}

// ---- log ------------------------------------------------------------------

fn level_style(level: &str) -> Style {
    Style::default().fg(match level {
        "error" => Color::Red,
        "warn" => Color::Yellow,
        _ => FAINT_TEXT,
    })
}

/// Wide enough for `Sep 24 14:02` and a gap.
const LOG_TIME_W: usize = 14;

fn log_row(line: &LogLine, now: i64) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            pad(&clock(line.at, now), LOG_TIME_W),
            Style::default().fg(DIM_TEXT),
        ),
        Span::styled(one_line(&line.text), level_style(&line.level)),
    ])
}

pub(super) fn render_log(
    frame: &mut Frame,
    body: Rect,
    agent: &AgentSnapshot,
    d: &Detail,
    scroll: &mut usize,
    now: i64,
) {
    if agent.events.is_empty() {
        render_empty(
            frame,
            body,
            "Nothing logged yet. Runs, poll failures, halts and changes made here land in events.jsonl.",
        );
        return;
    }
    let line = &agent.events[d.log_scroll.min(agent.events.len() - 1)];
    // The pane only earns its rows when the selected line doesn't fit.
    let fits = LOG_TIME_W + 1 + one_line(&line.text).chars().count() <= body.width as usize;
    let rows: Vec<Line<'static>> = agent.events.iter().map(|l| log_row(l, now)).collect();
    if fits {
        render_rows(frame, body, rows, d.log_scroll, scroll);
        return;
    }
    let (list, pane) = split_body(body, agent.events.len());
    render_rows(frame, list, rows, d.log_scroll, scroll);
    render_pane(
        frame,
        pane,
        vec![
            Line::from(Span::styled(
                clock(line.at, now),
                Style::default().fg(DIM_TEXT),
            )),
            Line::from(Span::styled(line.text.clone(), level_style(&line.level))),
        ],
    );
}

// ---- settings -------------------------------------------------------------

const LABEL_W: usize = 16;

pub(super) fn render_settings(
    frame: &mut Frame,
    body: Rect,
    agent: &AgentSnapshot,
    d: &Detail,
    scroll: &mut usize,
    now: i64,
) {
    let _ = now;
    let spec = match &agent.spec {
        Ok(spec) => spec,
        Err(e) => {
            let lines = vec![
                Line::from(Span::styled(e.clone(), Style::default().fg(Color::Red))),
                Line::default(),
                Line::from(Span::styled(
                    "Settings need a spec that loads. n opens Claude in the agent's folder to fix it.",
                    Style::default().fg(DIM_TEXT),
                )),
            ];
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
            return;
        }
    };
    let (list, pane) = split_body(body, SETTINGS.len());
    let label = Style::default().fg(LABEL_GRAY);
    let rows: Vec<Line<'static>> = SETTINGS
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let editing = (i == d.setting).then_some(d.editing.as_ref()).flatten();
            let value = match editing {
                Some(buf) => Span::styled(
                    format!("{}▏", buf),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                None if s.locked(spec).is_some() => {
                    Span::styled(s.show(spec), Style::default().fg(DIM_TEXT))
                }
                None => Span::styled(s.show(spec), setting_style(*s, spec)),
            };
            Line::from(vec![Span::styled(pad(s.label(), LABEL_W), label), value])
        })
        .collect();
    render_rows(frame, list, rows, d.setting, scroll);

    let setting = SETTINGS[d.setting.min(SETTINGS.len() - 1)];
    let dim = Style::default().fg(DIM_TEXT);
    let mut lines = vec![Line::from(Span::styled(
        setting
            .locked(spec)
            .map(|why| format!("Fixed: {}.", why))
            .unwrap_or_else(|| setting.help().to_string()),
        Style::default().fg(FAINT_TEXT),
    ))];
    if d.editing.is_some() {
        lines.push(Line::from(Span::styled("enter saves · esc cancels", dim)));
    }
    lines.push(Line::default());
    let trigger = match spec.trigger.kind {
        crate::harness::spec::TriggerKind::Poll => format!(
            "poll `{}` every {}",
            spec.trigger.command.as_deref().unwrap_or(""),
            crate::harness::spec::fmt_secs(spec.trigger.interval_s)
        ),
        _ => spec.trigger_label(),
    };
    let st = &agent.state;
    for (k, v) in [
        ("trigger", trigger),
        (
            "spent",
            format!(
                "{} today ({} runs) · {} in total",
                fmt_cost(st.today_cost()),
                st.today_ticks(),
                fmt_cost(st.cost_usd)
            ),
        ),
        ("workdir", tilde(&spec.workdir)),
        ("folder", tilde(&agent.dir)),
    ] {
        lines.push(Line::from(vec![
            Span::styled(pad(k, LABEL_W), dim),
            Span::styled(v, dim),
        ]));
    }
    render_pane(frame, pane, lines);
}

fn setting_style(s: Setting, spec: &crate::harness::Spec) -> Style {
    let on = Style::default().fg(Color::Green);
    match s {
        Setting::Enabled if spec.enabled => on,
        Setting::Enabled => Style::default().fg(Color::Yellow),
        _ => Style::default().fg(Color::White),
    }
}
