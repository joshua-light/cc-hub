//! Agent and model choice for new sessions: the model picker (`N`) and the
//! run's default agent (`A`).

use crate::agent::AgentConfig;
use crate::app::picker_list::{rank_rows, step, PickerRow, Searchable};
use crate::app::{App, View};
use crate::config;

/// State behind [`View::ModelPicker`]: where the new session will spawn
/// (captured at open time so a rescan can't move the target), the live fuzzy
/// query, selected coding agent, and its filtered model choices.
#[derive(Clone, Debug)]
pub struct ModelPickerState {
    pub cwd: String,
    pub agent_id: String,
    pub selected: usize,
    pub filter: String,
    pub rows: Vec<PickerRow>,
    pub choices: Vec<ModelPickerChoice>,
    agents: Vec<AgentConfig>,
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
        self.selected = step(self.selected, delta, self.agents.len());
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
        self.selected = step(self.selected, delta, self.rows.len());
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
        self.rows = rank_rows(
            &self.filter,
            self.choices.iter().map(|model| Searchable {
                label: &model.label,
                detail: &model.detail,
                id: None,
            }),
        );
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

#[cfg(all(test, unix))]
mod tests {
    use crate::app::test_support::{app_with, session, status};
    use crate::app::{Command, SessionsCommand};
    use crate::models::SessionState;

    #[test]
    fn open_model_picker_captures_cwd_and_spawn_uses_selected_model() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = app_with(vec![session(
                "sid-1",
                SessionState::Processing,
                Some("cc-agent-1"),
            )]);
            let effects = app.execute(Command::Sessions(SessionsCommand::OpenModelPicker));
            assert!(effects.is_empty());
            assert_eq!(app.view, crate::app::View::ModelPicker);
            let picker = app.model_picker.as_ref().expect("picker state");
            assert_eq!(picker.cwd, "/tmp/proj");
            assert_eq!(picker.selected, 0);

            app.model_picker_move(1);
            app.spawn_from_model_picker();
            assert_eq!(app.view, crate::app::View::Grid);
            assert!(app.model_picker.is_none());
            let spawns = runtime.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1);
            assert_eq!(
                spawns[0].model.as_deref(),
                Some(crate::agent::DEFAULT_CLAUDE_MODELS[1].1)
            );
            assert_eq!(spawns[0].cwd, "/tmp/proj");
            assert!(status(&app).starts_with("started"), "got: {}", status(&app));
        });
    }

    #[test]
    fn model_picker_move_clamps_to_list() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = app_with(vec![]);
            app.execute(Command::Sessions(SessionsCommand::OpenModelPicker));
            app.model_picker_move(-1);
            assert_eq!(app.model_picker.as_ref().unwrap().selected, 0);
            app.model_picker_move(100);
            assert_eq!(
                app.model_picker.as_ref().unwrap().selected,
                crate::agent::DEFAULT_CLAUDE_MODELS.len() - 1
            );
        });
    }

    #[test]
    fn model_picker_fuzzy_filters_labels_and_model_ids() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = app_with(vec![]);
            app.execute(Command::Sessions(SessionsCommand::OpenModelPicker));
            let picker = app.model_picker.as_mut().unwrap();

            for c in "sn5".chars() {
                picker.push_filter(c);
            }
            assert_eq!(picker.rows.len(), 1);
            assert_eq!(
                picker.selected_model(),
                Some((
                    crate::agent::DEFAULT_CLAUDE_MODELS[1].0,
                    Some(crate::agent::DEFAULT_CLAUDE_MODELS[1].1)
                ))
            );
            assert!(!picker.rows[0].label_indices.is_empty());

            for _ in 0..3 {
                picker.pop_filter();
            }
            for c in "-f".chars() {
                picker.push_filter(c);
            }
            assert_eq!(picker.rows.len(), 1);
            assert_eq!(
                picker.selected_model(),
                Some((
                    crate::agent::DEFAULT_CLAUDE_MODELS[2].0,
                    Some(crate::agent::DEFAULT_CLAUDE_MODELS[2].1)
                ))
            );
            assert!(!picker.rows[0].detail_indices.is_empty());
        });
    }

    #[test]
    fn model_picker_tab_cycles_agents_and_their_model_choices() {
        use crate::agent::{default_claude_models, AgentConfig, AgentKind, AgentModel};

        let mut picker = crate::app::ModelPickerState::new(
            "/tmp/proj".into(),
            "claude".into(),
            vec![
                AgentConfig {
                    id: "pi-codex".into(),
                    kind: AgentKind::Pi,
                    command: "pi --provider openai-codex".into(),
                    use_bridge: true,
                    models: vec![
                        AgentModel {
                            label: "GPT-5.6".into(),
                            id: "gpt-5.6".into(),
                        },
                        AgentModel {
                            label: "Sol".into(),
                            id: "sol".into(),
                        },
                    ],
                },
                AgentConfig {
                    id: "claude".into(),
                    kind: AgentKind::Claude,
                    command: "claude".into(),
                    use_bridge: false,
                    models: default_claude_models(),
                },
            ],
        );
        picker.push_filter('s');

        picker.cycle_agent();

        assert_eq!(picker.agent_id, "pi-codex");
        assert!(picker.filter.is_empty());
        assert_eq!(picker.rows.len(), 2);
        assert_eq!(picker.selected_model(), Some(("GPT-5.6", Some("gpt-5.6"))));

        picker.cycle_agent();
        assert_eq!(picker.agent_id, "claude");
        assert_eq!(picker.rows.len(), crate::agent::DEFAULT_CLAUDE_MODELS.len());
    }

    #[test]
    fn configured_agent_spawns_without_a_claude_model_override() {
        use crate::agent::{AgentConfig, AgentKind};

        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = app_with(vec![]);
            app.model_picker = Some(crate::app::ModelPickerState::new(
                "/tmp/proj".into(),
                "pi-codex".into(),
                vec![AgentConfig {
                    id: "pi-codex".into(),
                    kind: AgentKind::Pi,
                    command: "pi --provider openai-codex --model gpt-5.5".into(),
                    use_bridge: true,
                    models: Vec::new(),
                }],
            ));
            app.view = crate::app::View::ModelPicker;

            app.spawn_from_model_picker();

            let spawns = runtime.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1);
            assert_eq!(spawns[0].agent_id, "pi-codex");
            assert_eq!(spawns[0].model, None);
        });
    }

    #[test]
    fn configured_pi_model_is_forwarded_to_spawn() {
        use crate::agent::{AgentConfig, AgentKind, AgentModel};

        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = app_with(vec![]);
            app.model_picker = Some(crate::app::ModelPickerState::new(
                "/tmp/proj".into(),
                "pi-codex".into(),
                vec![AgentConfig {
                    id: "pi-codex".into(),
                    kind: AgentKind::Pi,
                    command: "pi --provider openai-codex".into(),
                    use_bridge: true,
                    models: vec![AgentModel {
                        label: "GPT-5.6".into(),
                        id: "gpt-5.6".into(),
                    }],
                }],
            ));
            app.view = crate::app::View::ModelPicker;

            app.spawn_from_model_picker();

            let spawns = runtime.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1);
            assert_eq!(spawns[0].agent_id, "pi-codex");
            assert_eq!(spawns[0].model.as_deref(), Some("gpt-5.6"));
        });
    }

    #[test]
    fn model_picker_no_match_does_not_spawn_or_close() {
        crate::test_util::with_temp_home(|| {
            let (mut app, runtime) = app_with(vec![]);
            app.execute(Command::Sessions(SessionsCommand::OpenModelPicker));
            for c in "xyz".chars() {
                app.model_picker.as_mut().unwrap().push_filter(c);
            }

            app.spawn_from_model_picker();

            assert_eq!(app.view, crate::app::View::ModelPicker);
            assert!(app.model_picker.is_some());
            assert!(runtime.spawns.lock().unwrap().is_empty());
        });
    }
}
