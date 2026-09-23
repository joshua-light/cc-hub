//! Agents-tab state: the latest on-disk snapshot of every persistent agent,
//! the selection cursor over the table (one row per agent), and the detail
//! view's cursors while it is open.

use crate::harness::AgentSnapshot;

#[derive(Default)]
pub struct HarnessView {
    pub agents: Vec<AgentSnapshot>,
    pub selected: usize,
    /// True once the first scan landed, so an empty tab can say "no agents"
    /// instead of "loading".
    pub loaded: bool,
    /// Whether the in-TUI supervisor is running (`[harness] enabled`).
    pub supervisor_on: bool,
    /// `Some` while the detail view is open, including under a transcript
    /// or pane opened from it, so closing those lands back in the detail.
    pub detail: Option<Detail>,
}

/// The detail view's sections, in strip order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    Runs,
    Artifacts,
    Log,
    Settings,
}

impl Section {
    pub const ALL: [Section; 4] = [
        Section::Runs,
        Section::Artifacts,
        Section::Log,
        Section::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Runs => "Runs",
            Section::Artifacts => "Artifacts",
            Section::Log => "Log",
            Section::Settings => "Settings",
        }
    }

    pub fn step(self, delta: isize) -> Section {
        let i = Self::ALL.iter().position(|s| *s == self).unwrap_or(0) as isize;
        let n = Self::ALL.len() as isize;
        Self::ALL[(i + delta).rem_euclid(n) as usize]
    }
}

/// One cursor per section, so flipping between them keeps your place.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Detail {
    pub section: Section,
    pub run: usize,
    pub artifact: usize,
    pub log_scroll: usize,
    pub setting: usize,
    /// The value being typed for the selected setting.
    pub editing: Option<String>,
}

impl HarnessView {
    pub fn update(&mut self, agents: Vec<AgentSnapshot>) {
        let keep = self.selected().map(|a| a.name.clone());
        self.agents = agents;
        self.loaded = true;
        self.selected = keep
            .and_then(|n| self.agents.iter().position(|a| a.name == n))
            .unwrap_or(0)
            .min(self.agents.len().saturating_sub(1));
    }

    pub fn selected(&self) -> Option<&AgentSnapshot> {
        self.agents.get(self.selected)
    }

    pub fn selected_mut(&mut self) -> Option<&mut AgentSnapshot> {
        self.agents.get_mut(self.selected)
    }

    pub fn nav(&mut self, delta: isize) {
        if self.agents.is_empty() {
            return;
        }
        let max = self.agents.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    /// Move the open section's cursor, clamped to what the section holds.
    pub fn detail_nav(&mut self, delta: isize) {
        let Some(agent) = self.agents.get(self.selected) else {
            return;
        };
        let (runs, notes, events) = (agent.runs().len(), agent.notes.len(), agent.events.len());
        let Some(d) = self.detail.as_mut() else {
            return;
        };
        let (cursor, len) = match d.section {
            Section::Runs => (&mut d.run, runs),
            Section::Artifacts => (&mut d.artifact, notes),
            Section::Log => (&mut d.log_scroll, events),
            Section::Settings => (&mut d.setting, crate::harness::settings::SETTINGS.len()),
        };
        let max = len.saturating_sub(1) as isize;
        *cursor = (*cursor as isize + delta).clamp(0, max) as usize;
    }

    /// Persistent agents that need the user: halted or broken.
    pub fn attention_count(&self) -> usize {
        self.agents
            .iter()
            .filter(|a| a.status().needs_attention())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_wrap_both_ways() {
        assert_eq!(Section::Runs.step(-1), Section::Settings);
        assert_eq!(Section::Settings.step(1), Section::Runs);
        assert_eq!(Section::Runs.step(2), Section::Log);
    }
}
