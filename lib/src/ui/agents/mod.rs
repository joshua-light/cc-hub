//! Agents tab: a row per persistent agent, and the detail view behind `f`.
//!
//! The row answers "is it on, and did its last run work?" at a glance. The
//! detail leads with the same answer in words — why it failed, why it is
//! halted — then splits the rest into sections (Runs, Artifacts, Log,
//! Settings), each a list with the selected item spelled out below it.
//!
//! - `table`: the tab body, one row per agent
//! - `detail`: the detail popup's headline, section strip and list/pane shell
//! - `sections`: the Runs, Artifacts, Log and Settings sections
//! - `text`: agent-specific text formatting

use crate::app::{App, Section, View};
use crate::harness::AgentStatus;
use crate::ui::palette::{MUTED_TEXT, PURPLE, SEP_GRAY};
use ratatui::style::Color;

mod detail;
mod sections;
mod table;
mod text;

pub(crate) use detail::render_agent_detail;
pub(crate) use table::render_agents_body;

const SELECTED_BG: Color = Color::Rgb(40, 40, 52);

fn status_indicator(status: AgentStatus) -> (&'static str, Color) {
    match status {
        AgentStatus::Ticking => ("󰒓", Color::Green),
        AgentStatus::Sleeping => ("󰒲", MUTED_TEXT),
        AgentStatus::Halted => ("󰂞", Color::Yellow),
        AgentStatus::Paused => ("󰏤", PURPLE),
        AgentStatus::Disabled => ("󰜎", SEP_GRAY),
        AgentStatus::Broken => ("󰅙", Color::Red),
    }
}

/// Key hints for the Agents tab and its detail.
pub(crate) fn hints(app: &App) -> &'static str {
    let Some(d) = app
        .harness
        .detail
        .as_ref()
        .filter(|_| app.view == View::AgentDetail)
    else {
        return "f/enter:open  space:on/off  p:run now  n:claude here  j/k:nav  tab:next  q:quit";
    };
    if d.editing.is_some() {
        return "type a value  enter:save  esc:cancel";
    }
    match d.section {
        Section::Runs => "f/enter:transcript  j/k:run  h/l:section  space:on/off  p:run now  n:claude here  R:reset  esc:close",
        Section::Artifacts => "f/enter:open  j/k:select  h/l:section  space:on/off  p:run now  n:claude here  esc:close",
        Section::Log => "j/k:scroll  h/l:section  space:on/off  p:run now  n:claude here  esc:close",
        Section::Settings => "enter:change  e:type value  j/k:field  h/l:section  space:on/off  n:claude here  esc:close",
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
