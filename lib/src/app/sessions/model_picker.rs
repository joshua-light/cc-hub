//! Agent and model choice for new sessions: the model picker (`N`) and the
//! run's default agent (`A`).

use crate::agent::AgentConfig;
use crate::app::{App, View};
use crate::config;

/// Default model choices for the implicit Claude agent.
pub use crate::agent::DEFAULT_CLAUDE_MODELS as SPAWN_MODELS;

/// State behind [`View::ModelPicker`]: where the new session will spawn
/// (captured at open time so a rescan can't move the target), the live fuzzy
/// query, selected coding agent, and its filtered model choices.
#[derive(Clone, Debug)]
pub struct ModelPickerState {
    pub cwd: String,
    pub agent_id: String,
    pub selected: usize,
    pub filter: String,
    pub rows: Vec<ModelPickerRow>,
    pub choices: Vec<ModelPickerChoice>,
    agents: Vec<AgentConfig>,
}

/// One visible model-picker row plus the character indices highlighted in the
/// label or detail. Only the better-scoring side is highlighted.
#[derive(Clone, Debug, Default)]
pub struct ModelPickerRow {
    pub choice: usize,
    pub label_indices: Vec<usize>,
    pub detail_indices: Vec<usize>,
}

/// A model choice for the currently-selected coding agent. Agents with no
/// configured models get one choice with no override, leaving the provider
/// and model in their command untouched.
#[derive(Clone, Debug)]
pub struct ModelPickerChoice {
    pub label: String,
    pub detail: String,
    pub model_id: Option<String>,
}

/// State behind [`View::AgentPicker`]. Agent ids are sorted so the picker is
/// stable regardless of TOML map iteration order.
#[derive(Clone, Debug)]
pub struct AgentPickerState {
    pub agents: Vec<AgentConfig>,
    pub selected: usize,
}

impl AgentPickerState {
    pub(crate) fn new(default_agent_id: &str, mut agents: Vec<AgentConfig>) -> Self {
        agents.sort_by(|a, b| a.id.cmp(&b.id));
        let selected = agents
            .iter()
            .position(|agent| agent.id == default_agent_id)
            .unwrap_or(0);
        Self { agents, selected }
    }

    pub fn move_selection(&mut self, delta: isize) {
        let last = self.agents.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    pub fn selected_agent(&self) -> Option<&AgentConfig> {
        self.agents.get(self.selected)
    }
}

impl ModelPickerState {
    pub(crate) fn new(cwd: String, default_agent_id: String, mut agents: Vec<AgentConfig>) -> Self {
        agents.sort_by(|a, b| a.id.cmp(&b.id));
        let agent_id = agents
            .iter()
            .find(|agent| agent.id == default_agent_id)
            .or_else(|| agents.first())
            .map(|agent| agent.id.clone())
            .unwrap_or(default_agent_id);
        let mut picker = Self {
            cwd,
            agent_id,
            selected: 0,
            filter: String::new(),
            rows: Vec::new(),
            choices: Vec::new(),
            agents,
        };
        picker.reload_choices();
        picker
    }

    pub fn push_filter(&mut self, c: char) {
        self.filter.push(c);
        self.refilter();
    }

    pub fn pop_filter(&mut self) {
        self.filter.pop();
        self.refilter();
    }

    pub fn move_selection(&mut self, delta: isize) {
        let last = self.rows.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    pub fn selected_model(&self) -> Option<(&str, Option<&str>)> {
        self.rows
            .get(self.selected)
            .and_then(|row| self.choices.get(row.choice))
            .map(|choice| (choice.label.as_str(), choice.model_id.as_deref()))
    }

    pub fn has_multiple_agents(&self) -> bool {
        self.agents.len() > 1
    }

    /// Tab: move to the next configured coding agent and rebuild the model
    /// choices appropriate for it. The old query is cleared because it was
    /// entered against a different candidate set.
    pub fn cycle_agent(&mut self) {
        if !self.has_multiple_agents() {
            return;
        }
        let current = self
            .agents
            .iter()
            .position(|agent| agent.id == self.agent_id)
            .unwrap_or(0);
        self.agent_id = self.agents[(current + 1) % self.agents.len()].id.clone();
        self.filter.clear();
        self.reload_choices();
    }

    fn reload_choices(&mut self) {
        self.choices = self
            .agents
            .iter()
            .find(|agent| agent.id == self.agent_id)
            .map(|agent| {
                if agent.models.is_empty() {
                    vec![ModelPickerChoice {
                        label: "Configured provider/model".into(),
                        detail: agent.command.clone(),
                        model_id: None,
                    }]
                } else {
                    agent
                        .models
                        .iter()
                        .map(|model| ModelPickerChoice {
                            label: model.label.clone(),
                            detail: model.id.clone(),
                            model_id: Some(model.id.clone()),
                        })
                        .collect()
                }
            })
            .unwrap_or_default();
        self.refilter();
    }

    fn refilter(&mut self) {
        if self.filter.is_empty() {
            self.rows = (0..self.choices.len())
                .map(|choice| ModelPickerRow {
                    choice,
                    ..Default::default()
                })
                .collect();
        } else {
            let mut scored = Vec::new();
            for (choice, model) in self.choices.iter().enumerate() {
                let label_match = crate::fuzzy::fuzzy_match(&self.filter, &model.label);
                let detail_match = crate::fuzzy::fuzzy_match(&self.filter, &model.detail);
                let label_score = label_match.as_ref().map(|m| m.score * 2);
                let detail_score = detail_match.as_ref().map(|m| m.score);
                let Some(score) = label_score.max(detail_score) else {
                    continue;
                };
                let row = if label_score >= detail_score {
                    ModelPickerRow {
                        choice,
                        label_indices: label_match.map(|m| m.indices).unwrap_or_default(),
                        detail_indices: Vec::new(),
                    }
                } else {
                    ModelPickerRow {
                        choice,
                        label_indices: Vec::new(),
                        detail_indices: detail_match.map(|m| m.indices).unwrap_or_default(),
                    }
                };
                scored.push((score, row));
            }
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.choice.cmp(&b.1.choice)));
            self.rows = scored.into_iter().map(|(_, row)| row).collect();
        }
        self.selected = 0;
    }
}

impl App {
    /// `N` on the Sessions tab: open the model picker for a new session in
    /// the selected session's cwd (falling back to `$HOME`). The target cwd
    /// and agent are captured now so a rescan can't move them under the
    /// popup; the spawn happens in [`Self::spawn_from_model_picker`].
    pub fn enter_model_picker(&mut self) {
        let Some(cwd) = self.default_spawn_cwd() else {
            self.set_status("no cwd to spawn in".into());
            return;
        };
        self.model_picker = Some(ModelPickerState::new(
            cwd,
            self.default_session_agent_id.clone(),
            config::get().resolved_agents().into_values().collect(),
        ));
        self.view = View::ModelPicker;
    }

    pub fn close_model_picker(&mut self) {
        self.model_picker = None;
        self.view = View::Grid;
    }

    /// `A` on the Sessions tab: choose the agent used by subsequent `n` and
    /// folder-picker new-session spawns. This is deliberately runtime state;
    /// the configured default remains the startup value.
    pub fn enter_agent_picker(&mut self) {
        let agents = config::get().resolved_agents().into_values().collect();
        self.agent_picker = Some(AgentPickerState::new(
            &self.default_session_agent_id,
            agents,
        ));
        self.view = View::AgentPicker;
    }

    pub fn close_agent_picker(&mut self) {
        self.agent_picker = None;
        self.view = View::Grid;
    }

    pub fn agent_picker_move(&mut self, delta: isize) {
        if let Some(picker) = self.agent_picker.as_mut() {
            picker.move_selection(delta);
        }
    }

    pub fn confirm_default_session_agent(&mut self) {
        let selected = self
            .agent_picker
            .as_ref()
            .and_then(AgentPickerState::selected_agent)
            .map(|agent| (agent.id.clone(), agent.display_label()));
        let Some((agent_id, label)) = selected else {
            return;
        };
        self.default_session_agent_id = agent_id.clone();
        self.close_agent_picker();
        self.set_status(format!("new sessions will use {} [{}]", label, agent_id));
    }

    pub fn default_session_agent_id(&self) -> &str {
        &self.default_session_agent_id
    }

    /// Move the model-picker highlight by `delta` rows, clamped to the live
    /// filtered result list.
    pub fn model_picker_move(&mut self, delta: isize) {
        if let Some(picker) = self.model_picker.as_mut() {
            picker.move_selection(delta);
        }
    }

    pub fn cycle_model_picker_agent(&mut self) {
        if let Some(picker) = self.model_picker.as_mut() {
            picker.cycle_agent();
        }
    }

    /// Enter on the model picker: spawn a fresh session in the captured cwd,
    /// applying the highlighted model override when that agent supports one,
    /// with the spawn watchdog armed (same contract as `n`).
    pub fn spawn_from_model_picker(&mut self) {
        let Some((label, model_id)) = self
            .model_picker
            .as_ref()
            .and_then(ModelPickerState::selected_model)
            .map(|(label, model_id)| (label.to_string(), model_id.map(str::to_string)))
        else {
            return;
        };
        let Some(picker) = self.model_picker.take() else {
            return;
        };
        self.view = View::Grid;
        let status = match self.runtime.spawn_session(
            &picker.agent_id,
            &picker.cwd,
            None,
            None,
            model_id.as_deref(),
            false,
        ) {
            Ok(name) => {
                let status = format!("started {} ({}) [{}]", picker.agent_id, label, name);
                self.watch_spawn(name, picker.agent_id, picker.cwd);
                status
            }
            Err(e) => format!("spawn failed: {}", e),
        };
        self.set_status(status);
    }
}
