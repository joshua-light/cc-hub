//! Agents tab: the agents down the side, the selected one's page beside
//! them; `f` gives the page the keyboard.
//!
//! A row answers "is it on, and did its last run work?" at a glance. The
//! page leads with the same answer in words — why it failed, why it is
//! halted — then splits the rest into sections (Runs, Artifacts, Log,
//! Settings), each a list with the selected item spelled out below it.
//!
//! - `list`: the tab body, and one sidebar row per agent
//! - `page`: the page's frame, headline, section strip and list/pane shell
//! - `sections`: the Runs, Artifacts, Log and Settings sections
//! - `text`: agent-specific text formatting

use crate::app::{App, Section, View};
use crate::harness::{AgentSnapshot, AgentStatus};
use crate::ui::common::Dot;
use ratatui::style::Style;
use ratatui::text::Span;

mod list;
mod page;
mod sections;
mod text;

pub(crate) use list::render_agents_body;

/// An agent's mark: a hollow ring while it is switched off, so off and on
/// differ in shape, not just shade; else its [`dot`].
fn mark(agent: &AgentSnapshot, now: u64) -> Span<'static> {
    match off(agent) {
        true => Span::styled(OFF_RING, Style::default().fg(Dot::Resting.steady())),
        false => dot(agent).span(now),
    }
}

/// The mark of an agent that won't run until you turn it back on.
const OFF_RING: &str = "○";

/// Switched off or paused: you said "not now".
fn off(agent: &AgentSnapshot) -> bool {
    matches!(agent.status(), AgentStatus::Paused | AgentStatus::Disabled)
}

/// An agent's dot, in the palette sessions use: white while it runs; red
/// when its spec won't load or its last run failed, halted or not; green
/// when a halt with nothing failed (a spent budget) waits for you; dark gray
/// asleep. Switched off, it is dark gray whatever happened, as a held
/// session is.
fn dot(agent: &AgentSnapshot) -> Dot {
    let failed = agent.last_run().is_some_and(|r| !r.ok);
    match agent.status() {
        AgentStatus::Ticking => Dot::Working,
        AgentStatus::Paused | AgentStatus::Disabled => Dot::Resting,
        AgentStatus::Broken => Dot::Failed,
        _ if failed => Dot::Failed,
        AgentStatus::Halted => Dot::Waiting,
        AgentStatus::Sleeping => Dot::Resting,
    }
}

/// Key hints for the Agents list and the page.
pub(crate) fn hints(app: &App) -> &'static str {
    if app.view != View::AgentDetail {
        return "f/enter:page  space:on/off  p:run now  n:claude here  j/k:agent  tab:next  q:quit";
    }
    let d = &app.harness.detail;
    if d.editing.is_some() {
        return "type a value  enter:save  esc:cancel";
    }
    match d.section {
        Section::Runs => "f/enter:transcript  j/k:run  h/l:section  space:on/off  p:run now  n:claude here  R:reset  esc/F1:list",
        Section::Artifacts => "f/enter:open  j/k:select  h/l:section  space:on/off  p:run now  n:claude here  esc/F1:list",
        Section::Log => "j/k:scroll  h/l:section  space:on/off  p:run now  n:claude here  esc/F1:list",
        Section::Settings => "enter:change  e:type value  j/k:field  h/l:section  space:on/off  n:claude here  esc/F1:list",
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{snap, ticked};
    use super::*;

    // A halt that came from failing runs is red, like any failed last run;
    // a halt with nothing failed waits for you in green; off is gray.
    #[test]
    fn a_failed_run_is_red_halted_or_not() {
        let mut halted = ticked(false);
        halted.stopped_reason = Some("5 consecutive failed ticks".into());
        assert_eq!(dot(&snap("repair", halted)), Dot::Failed);
        assert_eq!(dot(&snap("a", ticked(false))), Dot::Failed);

        let mut spent = ticked(true);
        spent.stopped_reason = Some("budget".into());
        assert_eq!(dot(&snap("b", spent)), Dot::Waiting);
        assert_eq!(dot(&snap("c", ticked(true))), Dot::Resting);

        let mut off = snap("d", ticked(false));
        if let Ok(spec) = off.spec.as_mut() {
            spec.enabled = false;
        }
        assert_eq!(dot(&off), Dot::Resting);
        assert_eq!(mark(&off, 0).content, OFF_RING);
        assert_eq!(mark(&snap("c", ticked(true)), 0).content, "●");
    }
}

#[cfg(test)]
mod fixtures {
    use crate::harness::{AgentSnapshot, AgentState, LogLine, Note, TickRecord};
    use std::path::Path;

    pub(super) const NOW: i64 = 1_800_000_000;

    pub(super) fn snap(name: &str, state: AgentState) -> AgentSnapshot {
        let dir = Path::new("/tmp/agents").join(name);
        let spec = crate::harness::spec::parse(
            &dir,
            "description = \"Watches things\"\n[trigger]\nkind = \"poll\"\ncommand = \"./watch.sh\"\ninterval_s = 300\n[run]\ndaily_budget_usd = 5.0\n[prompt]\ninstruction = \"go\"",
        );
        AgentSnapshot {
            name: name.into(),
            dir,
            spec,
            state,
            notes: vec![Note {
                at: NOW - 120,
                level: "warn".into(),
                text: "PR #418 lint fails".into(),
                r#ref: Some("https://example.com/pr/418".into()),
                tick: 3,
            }],
            events: vec![LogLine {
                at: NOW - 60,
                level: "warn".into(),
                text: "poll `./watch.sh` failed (exit status: 1): token expired".into(),
            }],
            inbox_pending: 2,
        }
    }

    pub(super) fn rec(ok: bool, result: &str) -> TickRecord {
        TickRecord {
            at: NOW - 300,
            event: Some("20260903T191749.728Z-poke".into()),
            ok,
            subtype: Some(
                if ok {
                    "success"
                } else {
                    "error_max_budget_usd"
                }
                .into(),
            ),
            turns: 4,
            compactions: 0,
            cost_usd: 0.07,
            context_start: 4000,
            context_end: 13_000,
            duration_s: 12,
            session_id: Some("sid-1".into()),
            result: result.into(),
            detail: (!ok).then(|| "exit 1 · boom".into()),
        }
    }

    pub(super) fn ticked(ok: bool) -> AgentState {
        AgentState {
            ticks: 3,
            cost_usd: 0.31,
            last_tick_at: Some(NOW - 300),
            last_result: "NOCHANGE".into(),
            history: vec![rec(ok, if ok { "NOCHANGE" } else { "ran out of budget" })],
            ..Default::default()
        }
    }
}
