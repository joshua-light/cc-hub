use crate::app::harness_view::Section;
use crate::app::App;

pub(super) const BROKEN_SPEC: &str =
    "agent.toml doesn't load — n opens Claude in its folder to fix it";

impl App {
    /// Open the edit box on the selected setting, pre-filled.
    pub(super) fn harness_edit_setting(&mut self) {
        let (Some(agent), Some(d)) = (self.harness.selected(), self.harness.detail.as_ref()) else {
            return;
        };
        if d.section != Section::Settings {
            return;
        }
        let Some(setting) = crate::harness::settings::SETTINGS.get(d.setting).copied() else {
            return;
        };
        let Ok(spec) = &agent.spec else {
            self.set_status(BROKEN_SPEC.into());
            return;
        };
        if let Some(why) = setting.locked(spec) {
            self.set_status(format!("{}: {}", setting.label(), why));
            return;
        }
        let raw = setting.raw(spec);
        if let Some(d) = self.harness.detail.as_mut() {
            d.editing = Some(raw);
        }
    }

    /// Write `input` to the selected setting. True when it was saved.
    pub(super) fn harness_apply_setting(&mut self, input: &str) -> bool {
        use crate::harness::{self, settings};
        let (Some(agent), Some(d)) = (self.harness.selected(), self.harness.detail.as_ref()) else {
            return false;
        };
        let Some(setting) = settings::SETTINGS.get(d.setting).copied() else {
            return false;
        };
        if setting == settings::Setting::Enabled {
            return match input.trim() {
                "on" | "true" => self.harness_set_on(true),
                "off" | "false" => self.harness_set_on(false),
                _ => {
                    self.set_status("enabled: on or off".into());
                    false
                }
            };
        }
        let (name, dir) = (agent.name.clone(), agent.dir.clone());
        match settings::apply(&dir, setting, input) {
            Ok(spec) => {
                let shown = setting.show(&spec);
                harness::log_event(
                    &dir,
                    "info",
                    format!("{} → {} (from the hub)", setting.label(), shown),
                );
                self.set_status(format!("{}: {} → {}", name, setting.label(), shown));
                if let Some(a) = self.harness.selected_mut() {
                    a.spec = Ok(spec);
                }
                true
            }
            Err(e) => {
                self.set_status(format!("{}: {}", name, e));
                false
            }
        }
    }

    /// Space: off when it would run, on otherwise — turning on also clears
    /// a halt or a CLI pause, so one key means "make it go".
    pub(super) fn harness_toggle_on(&mut self) {
        use crate::harness::AgentStatus as S;
        let Some(agent) = self.harness.selected() else {
            return;
        };
        let on = !matches!(agent.status(), S::Sleeping | S::Ticking);
        self.harness_set_on(on);
    }

    fn harness_set_on(&mut self, on: bool) -> bool {
        use crate::harness::{self, settings};
        let Some(agent) = self.harness.selected() else {
            return false;
        };
        let (name, dir) = (agent.name.clone(), agent.dir.clone());
        let Ok(spec) = &agent.spec else {
            self.set_status(format!("{}: {}", name, BROKEN_SPEC));
            return false;
        };
        let ticking = agent.state.ticking.is_some();
        let held = agent.state.paused || agent.state.stopped_reason.is_some();
        let mut new_spec = None;
        if spec.enabled != on {
            match settings::set_enabled(&dir, on) {
                Ok(s) => new_spec = Some(s),
                Err(e) => {
                    self.set_status(format!("{}: {}", name, e));
                    return false;
                }
            }
        }
        if on && held {
            if let Err(e) = harness::set_paused(&dir, false) {
                self.set_status(format!("{}: {}", name, e));
                return false;
            }
        }
        let what = if on { "on" } else { "off" };
        harness::log_event(&dir, "info", format!("turned {} from the hub", what));
        // Reflect it now; the next disk scan confirms it.
        if let Some(a) = self.harness.selected_mut() {
            if let Some(s) = new_spec {
                a.spec = Ok(s);
            }
            if on {
                a.state.paused = false;
                a.state.stopped_reason = None;
                a.state.failures_in_a_row = 0;
            }
        }
        let msg = if !on && ticking {
            format!("{}: off — the current run finishes first", name)
        } else {
            format!("{}: {}", name, what)
        };
        self.set_status(msg);
        true
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::test_support::{app_with_agent, harness, status};
    use crate::app::HarnessCommand;

    fn open_settings_at(app: &mut App, setting: crate::harness::settings::Setting) {
        harness(app, HarnessCommand::OpenDetail);
        harness(app, HarnessCommand::ShowSection(Section::Settings));
        let i = crate::harness::settings::SETTINGS
            .iter()
            .position(|s| *s == setting)
            .unwrap();
        app.harness.detail.as_mut().unwrap().setting = i;
    }

    #[test]
    fn space_turns_an_agent_off_and_on_again_clearing_a_halt() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt, dir) = app_with_agent();
            harness(&mut app, HarnessCommand::ToggleOn);
            assert!(!crate::harness::spec::load(&dir).unwrap().enabled);
            assert_eq!(
                app.harness.selected().unwrap().status(),
                crate::harness::AgentStatus::Disabled
            );

            crate::harness::update_state(&dir, |s| {
                s.stopped_reason = Some("daily budget $1 reached".into())
            })
            .unwrap();
            app.harness.update(vec![crate::harness::snapshot(&dir)]);
            harness(&mut app, HarnessCommand::ToggleOn);
            assert!(crate::harness::spec::load(&dir).unwrap().enabled);
            assert_eq!(crate::harness::load_state(&dir).stopped_reason, None);
            assert_eq!(status(&app), "bb-prs: on");
            let log = crate::harness::read_events(&dir, 10);
            assert_eq!(log[0].text, "turned on from the hub");
        });
    }

    #[test]
    fn space_resumes_a_halted_agent_instead_of_turning_it_off() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt, dir) = app_with_agent();
            crate::harness::update_state(&dir, |s| s.stopped_reason = Some("5 failed".into()))
                .unwrap();
            app.harness.update(vec![crate::harness::snapshot(&dir)]);
            harness(&mut app, HarnessCommand::ToggleOn);
            assert!(crate::harness::spec::load(&dir).unwrap().enabled);
            assert_eq!(crate::harness::load_state(&dir).stopped_reason, None);
        });
    }

    #[test]
    fn enter_on_model_steps_to_the_next_and_saves_it() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt, dir) = app_with_agent();
            open_settings_at(&mut app, crate::harness::settings::Setting::Model);
            harness(&mut app, HarnessCommand::Activate);
            let raw = std::fs::read_to_string(dir.join("agent.toml")).unwrap();
            assert!(raw.contains("model = \"haiku\""), "{raw}");
            assert!(raw.contains("# per run"), "comments survive: {raw}");
            assert_eq!(status(&app), "bb-prs: model → haiku");
            // The in-memory snapshot follows without waiting for a rescan.
            let spec = app.harness.selected().unwrap().spec.as_ref().unwrap();
            assert_eq!(spec.run.model.as_deref(), Some("haiku"));
        });
    }

    #[test]
    fn typed_value_saves_and_a_rejected_one_keeps_the_box_open() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt, dir) = app_with_agent();
            open_settings_at(&mut app, crate::harness::settings::Setting::RunBudget);
            // Free text: Enter opens the box pre-filled with the value.
            harness(&mut app, HarnessCommand::Activate);
            let d = app.harness.detail.as_ref().unwrap();
            assert_eq!(d.editing.as_deref(), Some("0.5"));

            for _ in 0..3 {
                harness(&mut app, HarnessCommand::EditBackspace);
            }
            harness(&mut app, HarnessCommand::EditChar('x'));
            harness(&mut app, HarnessCommand::EditSubmit);
            assert!(app.harness.detail.as_ref().unwrap().editing.is_some());
            assert!(status(&app).contains("dollar amount"), "{}", status(&app));

            harness(&mut app, HarnessCommand::EditBackspace);
            for c in "1.25".chars() {
                harness(&mut app, HarnessCommand::EditChar(c));
            }
            harness(&mut app, HarnessCommand::EditSubmit);
            assert!(app.harness.detail.as_ref().unwrap().editing.is_none());
            let spec = crate::harness::spec::load(&dir).unwrap();
            assert_eq!(spec.run.max_budget_usd, Some(1.25));
        });
    }
}
