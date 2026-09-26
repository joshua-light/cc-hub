use super::harness_settings::BROKEN_SPEC;
use super::Effect;
use crate::agent::AgentKind;
use crate::app::harness_view::{Detail, Section};
use crate::app::App;
use crate::config;
use crate::{models, spawn, title};

/// Agents-tab commands (persistent agents, `lib/src/harness/`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HarnessCommand {
    NavUp,
    NavDown,
    /// `f`/Enter — the detail view.
    OpenDetail,
    CloseDetail,
    /// Tab/`l` and Shift-Tab/`h` in the detail view.
    NextSection,
    PrevSection,
    /// `1`–`4` in the detail view.
    ShowSection(Section),
    /// `j`/`k` in the detail view: the open section's cursor.
    DetailDown,
    DetailUp,
    /// `f`/Enter in the detail view: the selected run's transcript, the
    /// selected artifact, or change the selected setting.
    Activate,
    /// `e` on a setting — type a value instead of stepping through them.
    EditSetting,
    EditChar(char),
    EditBackspace,
    EditSubmit,
    EditCancel,
    /// Space — turn the agent off, or on (which also clears a halt).
    ToggleOn,
    /// `p` — queue an empty event so the agent runs now.
    RunNow,
    /// `n` — an interactive Claude session in the agent's folder, primed
    /// to change it.
    NewSession,
    /// `R` in the detail view — clear harness bookkeeping (workdir
    /// untouched).
    Reset,
}

impl App {
    pub(super) fn execute_harness(&mut self, cmd: HarnessCommand) -> Vec<Effect> {
        use crate::app::View;
        use crate::harness;
        use HarnessCommand::*;
        match cmd {
            NavUp => self.harness.nav(-1),
            NavDown => self.harness.nav(1),
            OpenDetail => {
                if self.harness.selected().is_some() {
                    self.harness.detail = Some(Detail::default());
                    self.render.agent_detail_scroll = 0;
                    self.view = View::AgentDetail;
                }
            }
            CloseDetail => {
                self.harness.detail = None;
                self.view = View::Grid;
            }
            NextSection | PrevSection | ShowSection(_) => {
                if let Some(d) = self.harness.detail.as_mut() {
                    d.section = match cmd {
                        NextSection => d.section.step(1),
                        PrevSection => d.section.step(-1),
                        ShowSection(s) => s,
                        _ => d.section,
                    };
                    d.editing = None;
                    self.render.agent_detail_scroll = 0;
                }
            }
            DetailDown => self.harness.detail_nav(1),
            DetailUp => self.harness.detail_nav(-1),
            Activate => return self.harness_activate(),
            EditSetting => self.harness_edit_setting(),
            EditChar(c) => {
                if let Some(buf) = self.harness_edit_buffer() {
                    buf.push(c);
                }
            }
            EditBackspace => {
                if let Some(buf) = self.harness_edit_buffer() {
                    buf.pop();
                }
            }
            EditCancel => {
                if let Some(d) = self.harness.detail.as_mut() {
                    d.editing = None;
                }
            }
            EditSubmit => {
                let Some(input) = self.harness.detail.as_ref().and_then(|d| d.editing.clone())
                else {
                    return Vec::new();
                };
                // A rejected value keeps the box open so it can be fixed.
                if self.harness_apply_setting(&input) {
                    if let Some(d) = self.harness.detail.as_mut() {
                        d.editing = None;
                    }
                }
            }
            ToggleOn => self.harness_toggle_on(),
            RunNow => self.harness_run_now(),
            NewSession => return self.harness_new_session(),
            Reset => {
                let Some(agent) = self.harness.selected() else {
                    return Vec::new();
                };
                let (name, dir) = (agent.name.clone(), agent.dir.clone());
                let msg = match harness::reset(&dir) {
                    Ok(()) => {
                        harness::log_event(&dir, "info", "state reset from the hub");
                        format!("{}: state reset (workdir untouched)", name)
                    }
                    Err(e) => format!("{}: reset failed: {}", name, e),
                };
                if let Some(a) = self.harness.selected_mut() {
                    a.state = Default::default();
                }
                self.set_status(msg);
            }
        }
        Vec::new()
    }

    fn harness_edit_buffer(&mut self) -> Option<&mut String> {
        self.harness.detail.as_mut()?.editing.as_mut()
    }

    /// `f`/Enter inside the detail view, by section.
    fn harness_activate(&mut self) -> Vec<Effect> {
        use crate::harness::Run;
        let (Some(agent), Some(d)) = (self.harness.selected(), self.harness.detail.as_ref()) else {
            return Vec::new();
        };
        match d.section {
            Section::Runs => {
                let runs = agent.runs();
                let Some(run) = runs.get(d.run) else {
                    let msg = format!("{}: no runs yet — p runs it now", agent.name);
                    self.set_status(msg);
                    return Vec::new();
                };
                let n = run.n();
                let Some(sid) = run.session_id().map(str::to_string) else {
                    let msg = match run {
                        Run::InFlight { .. } => {
                            format!("run #{}: starting — its transcript appears in a moment", n)
                        }
                        Run::Done { .. } => format!("run #{}: no transcript recorded", n),
                    };
                    self.set_status(msg);
                    return Vec::new();
                };
                let Some(path) = agent.transcript(&sid) else {
                    self.set_status(format!(
                        "run #{}: transcript {} not found",
                        n,
                        models::short_sid(&sid)
                    ));
                    return Vec::new();
                };
                let lv = crate::live_view::LiveView::new(path.clone(), AgentKind::Claude);
                if lv.messages.is_empty() {
                    self.set_status(format!("run #{}: {} is empty", n, path.display()));
                } else {
                    self.enter_live_tail(lv);
                }
                Vec::new()
            }
            Section::Artifacts => {
                let Some(note) = agent.notes.get(d.artifact) else {
                    let msg = format!("{}: nothing reported yet", agent.name);
                    self.set_status(msg);
                    return Vec::new();
                };
                match note.r#ref.clone() {
                    Some(target) => {
                        self.set_status(format!("opening {}", target));
                        vec![Effect::OpenExternal { target }]
                    }
                    None => {
                        self.set_status("this note points at nothing to open".into());
                        Vec::new()
                    }
                }
            }
            Section::Log => Vec::new(),
            Section::Settings => {
                let Some(setting) = crate::harness::settings::SETTINGS.get(d.setting).copied()
                else {
                    return Vec::new();
                };
                let Ok(spec) = &agent.spec else {
                    self.set_status(BROKEN_SPEC.into());
                    return Vec::new();
                };
                if let Some(why) = setting.locked(spec) {
                    self.set_status(format!("{}: {}", setting.label(), why));
                    return Vec::new();
                }
                match setting.step(spec) {
                    Some(next) => {
                        self.harness_apply_setting(&next);
                    }
                    None => self.harness_edit_setting(),
                }
                Vec::new()
            }
        }
    }

    /// `p`: queue an empty event, and say when it will actually run.
    fn harness_run_now(&mut self) {
        use crate::harness::{self, AgentStatus as S};
        let Some(agent) = self.harness.selected() else {
            return;
        };
        let (name, dir, status) = (agent.name.clone(), agent.dir.clone(), agent.status());
        if let Err(e) = harness::poke(&dir, "") {
            self.set_status(format!("{}: couldn't queue a run: {}", name, e));
            return;
        }
        let when = match status {
            _ if !self.harness.supervisor_on => {
                "queued — but the supervisor is off ([harness] enabled in config.toml)"
            }
            S::Sleeping => "starting a run",
            S::Ticking => "queued after the current run",
            S::Disabled | S::Paused => "queued — runs once you turn it on (space)",
            S::Halted => "queued — it is halted; space resumes it",
            S::Broken => "queued — but agent.toml doesn't load",
        };
        self.set_status(format!("{}: {}", name, when));
    }

    /// `n`: a Claude session in the agent's folder, told what the folder is
    /// and how the agent last fared, then attached — the shortest path from
    /// "that run failed" to "fix it".
    fn harness_new_session(&mut self) -> Vec<Effect> {
        let Some(agent) = self.harness.selected() else {
            return Vec::new();
        };
        let cfg = config::get();
        let Some(agent_id) = [self.default_session_agent_id.as_str(), "claude"]
            .into_iter()
            .find(|id| cfg.agent(id).is_some_and(|a| a.kind == AgentKind::Claude))
            .map(str::to_string)
        else {
            self.set_status("no Claude agent configured".into());
            return Vec::new();
        };
        let name = agent.name.clone();
        let cwd = agent.dir.to_string_lossy().into_owned();
        let prompt = edit_prompt(agent);
        let sid = uuid::Uuid::new_v4().to_string();
        if let Err(e) = title::persist_title(&sid, &format!("agent: {}", name)) {
            log::warn!("agents: couldn't title the edit session: {}", e);
        }
        match self.runtime.spawn_session(
            &agent_id,
            &cwd,
            Some(spawn::SessionTarget::Fresh(sid)),
            Some(&prompt),
            None,
            false,
        ) {
            Ok(tmux) => {
                self.set_status(format!("{}: Claude in {} — F1 detaches", name, cwd));
                vec![Effect::OpenTmuxPane { tmux, owned: false }]
            }
            Err(e) => {
                self.set_status(format!("{}: spawn failed: {}", name, e));
                Vec::new()
            }
        }
    }
}

/// The opening message of an `n` session: what the folder is, and what is
/// wrong with the agent right now, so "fix it" is a complete instruction.
fn edit_prompt(agent: &crate::harness::AgentSnapshot) -> String {
    let name = &agent.name;
    let mut p = format!(
        "You're working on `{name}`, a cc-hub persistent agent: a headless `claude -p` \
that wakes on its trigger and runs one bounded turn. This directory is the agent:
- agent.toml — its spec: trigger, model, tools, budgets, prompts. Edits apply on the next run.
- work/ — its default working directory and its only memory between runs.
- notes.jsonl — what it reported. events.jsonl — the harness log: runs, poll failures, halts.
- log/ — raw stream-json of every run, one file per day.
- state.json — harness bookkeeping. Don't edit it.
`cc-hub agent once {name}` runs it once and prints the outcome.\n"
    );
    let trouble = match (&agent.spec, &agent.state.stopped_reason, agent.last_run()) {
        (Err(e), _, _) => Some(format!("agent.toml doesn't load: {}", e)),
        (_, Some(reason), _) => Some(format!("It is halted: {}.", reason)),
        (_, _, Some(run)) if !run.ok => Some(format!(
            "Its last run failed ({}): {}{}",
            run.subtype.as_deref().unwrap_or("error"),
            run.result,
            run.detail
                .as_deref()
                .map(|d| format!("\nstderr: {}", d))
                .unwrap_or_default()
        )),
        _ => None,
    };
    if let Some(t) = trouble {
        p.push('\n');
        p.push_str(&t);
        p.push('\n');
    }
    p.push_str("\nRead agent.toml, then wait for my instructions.");
    p
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::test_support::{app_with_agent, harness, status};

    #[test]
    fn f_opens_the_agent_and_esc_closes_it() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt, _dir) = app_with_agent();
            harness(&mut app, HarnessCommand::OpenDetail);
            assert_eq!(app.view, crate::app::View::AgentDetail);
            assert_eq!(app.harness.detail.as_ref().unwrap().section, Section::Runs);
            harness(&mut app, HarnessCommand::NextSection);
            assert_eq!(
                app.harness.detail.as_ref().unwrap().section,
                Section::Artifacts
            );
            harness(&mut app, HarnessCommand::CloseDetail);
            assert_eq!(app.view, crate::app::View::Grid);
            assert!(app.harness.detail.is_none());
        });
    }

    #[test]
    fn p_queues_a_run() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt, dir) = app_with_agent();
            harness(&mut app, HarnessCommand::RunNow);
            assert_eq!(
                crate::harness::trigger::pending_count(&crate::harness::inbox_path(&dir)),
                1
            );
            assert_eq!(status(&app), "bb-prs: starting a run");
        });
    }

    #[test]
    fn n_opens_claude_in_the_agent_folder_and_attaches_it() {
        crate::test_util::with_temp_home(|| {
            let (mut app, rt, dir) = app_with_agent();
            crate::harness::update_state(&dir, |s| {
                s.history.push(crate::harness::TickRecord {
                    at: 0,
                    event: None,
                    session_id: None,
                    ok: false,
                    subtype: Some("error_max_turns".into()),
                    turns: 40,
                    compactions: 0,
                    cost_usd: 0.1,
                    context_start: 0,
                    context_end: 0,
                    duration_s: 30,
                    result: "hit the turn cap".into(),
                    detail: None,
                })
            })
            .unwrap();
            app.harness.update(vec![crate::harness::snapshot(&dir)]);

            let effects = harness(&mut app, HarnessCommand::NewSession);
            assert_eq!(
                effects,
                vec![Effect::OpenTmuxPane {
                    tmux: "mock-spawn".into(),
                    owned: false
                }]
            );
            let spawns = rt.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1);
            assert_eq!(spawns[0].cwd, dir.to_string_lossy());
            assert!(spawns[0].resume.as_deref().unwrap().contains("Fresh"));
            let prompt = spawns[0].initial_prompt.as_deref().unwrap();
            assert!(prompt.contains("`bb-prs`"), "{prompt}");
            assert!(
                prompt.contains("last run failed (error_max_turns): hit the turn cap"),
                "{prompt}"
            );
        });
    }

    #[test]
    fn closing_a_transcript_lands_back_in_the_agent_detail() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt, _dir) = app_with_agent();
            harness(&mut app, HarnessCommand::OpenDetail);
            app.view = crate::app::View::LiveTail;
            app.close_live_tail();
            assert_eq!(app.view, crate::app::View::AgentDetail);

            harness(&mut app, HarnessCommand::CloseDetail);
            app.view = crate::app::View::LiveTail;
            app.close_live_tail();
            assert_eq!(app.view, crate::app::View::Grid);
        });
    }
}
