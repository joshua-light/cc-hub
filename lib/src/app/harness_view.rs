//! Agents-tab state: the latest on-disk snapshot of every persistent agent
//! and the selection cursor over the table (one row per agent).

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

    pub fn nav(&mut self, delta: isize) {
        if self.agents.is_empty() {
            return;
        }
        let max = self.agents.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    /// Persistent agents that need the user: halted or broken.
    pub fn attention_count(&self) -> usize {
        self.agents
            .iter()
            .filter(|a| a.status().needs_attention())
            .count()
    }
}
