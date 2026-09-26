use super::Effect;
use crate::app::App;

/// Builds-tab commands (`lib/src/builds/`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildsCommand {
    NavUp,
    NavDown,
    NavLeft,
    NavRight,
    /// `n` — the new-build form for the selected recipe, seeded from its
    /// last build.
    OpenForm,
    /// Tab / Shift-Tab and ↓/↑ in the form: the next field.
    FormNext,
    FormPrev,
    /// ←/→ on a stepped field: its previous or next value.
    FormLeft,
    FormRight,
    FormChar(char),
    FormBackspace,
    FormSubmit,
    FormCancel,
    /// `r` — build the selected recipe's checkout as it is now.
    Rebuild,
    /// `c` — cancel the selected recipe's running and queued builds.
    Cancel,
    /// `b` — serve the build in the selected recipe's player.
    Serve,
    /// Space — reserve the selected recipe's resource, or let it go when the
    /// tab holds or waits for it.
    ToggleHold,
    /// Enter/`f` — the output of the build the selected card shows.
    OpenLog,
    CloseLog,
    LogUp,
    LogDown,
    LogPageUp,
    LogPageDown,
    LogEnd,
}

/// The first eleven characters of a commit, as `git log --oneline` shows it.
pub(crate) fn short_commit(commit: Option<&str>) -> Option<&str> {
    commit.map(|c| &c[..c.len().min(11)])
}

impl App {
    pub(super) fn execute_builds(&mut self, cmd: BuildsCommand) -> Vec<Effect> {
        use crate::app::{BuildForm, LogView, View};
        use crate::builds::{self, hold, recipe};
        use BuildsCommand::*;
        let cols = self.render.builds_cols.max(1) as isize;
        match cmd {
            NavLeft => self.builds.nav(-1),
            NavRight => self.builds.nav(1),
            NavUp => self.builds.nav(-cols),
            NavDown => self.builds.nav(cols),
            OpenForm => {
                let Some(name) = self.builds_recipe() else {
                    return Vec::new();
                };
                let like = self.builds.builds_of(&name).next();
                self.builds.form = Some(BuildForm::new(&name, like));
                self.view = View::BuildForm;
            }
            FormNext | FormPrev | FormLeft | FormRight | FormChar(_) | FormBackspace => {
                let Some(form) = self.builds.form.as_mut() else {
                    return Vec::new();
                };
                match cmd {
                    FormNext => form.step_field(1),
                    FormPrev => form.step_field(-1),
                    FormLeft => form.step_value(-1),
                    FormRight => form.step_value(1),
                    FormChar(c) => match form.text_mut() {
                        Some(text) => text.push(c),
                        None if c == ' ' => form.step_value(1),
                        None => {}
                    },
                    FormBackspace => {
                        if let Some(text) = form.text_mut() {
                            text.pop();
                        }
                    }
                    _ => {}
                }
            }
            FormCancel => {
                self.builds.form = None;
                self.view = View::Grid;
            }
            FormSubmit => {
                let Some(build) = self.builds.form.as_ref().map(|f| f.to_build()) else {
                    return Vec::new();
                };
                if build.cwd.is_empty() {
                    self.set_status("a build needs a checkout".into());
                    return Vec::new();
                }
                // A refused build keeps the form open, so it can be fixed.
                if self.builds_start(builds::start(build)) {
                    self.builds.form = None;
                    self.view = View::Grid;
                }
            }
            Rebuild => {
                let Some(name) = self.builds_recipe() else {
                    return Vec::new();
                };
                let started = match self.builds.builds_of(&name).next() {
                    Some(last) => builds::rebuild(&last.id),
                    None => builds::fresh(&name),
                };
                self.builds_start(started);
            }
            Cancel => {
                let Some(name) = self.builds_recipe() else {
                    return Vec::new();
                };
                let ids: Vec<String> = self
                    .builds
                    .active(&name)
                    .iter()
                    .map(|b| b.id.clone())
                    .collect();
                if ids.is_empty() {
                    self.set_status(format!("{}: nothing is building", name));
                    return Vec::new();
                }
                let failed: Vec<String> = ids
                    .iter()
                    .filter_map(|id| builds::cancel(id).err().map(|e| e.to_string()))
                    .collect();
                let msg = match (failed.first(), ids.len()) {
                    (Some(e), _) => format!("cancel failed: {}", e),
                    (None, 1) => format!("{}: cancelling", name),
                    (None, n) => format!("{}: cancelling {} builds", name, n),
                };
                self.set_status(msg);
            }
            Serve => {
                let Some(name) = self.builds_recipe() else {
                    return Vec::new();
                };
                let knows_current = recipe::named(&name).is_some_and(|r| !r.current.is_empty());
                // Without a `current` there is no telling what the player runs,
                // so the newest success is the best guess.
                let build = if knows_current {
                    self.builds.in_player(&name)
                } else {
                    self.builds.last_success(&name)
                };
                let msg = match build {
                    Some(b) => {
                        let label = short_commit(b.commit.as_deref())
                            .unwrap_or(b.target())
                            .to_string();
                        match builds::serve(&b.id) {
                            Ok(()) => format!("serving {}", label),
                            Err(e) => format!("serve failed: {}", e),
                        }
                    }
                    None if knows_current => {
                        format!("{}: the player was not built from here; r builds it", name)
                    }
                    None => format!("{}: nothing built yet; r builds it", name),
                };
                self.set_status(msg);
            }
            ToggleHold => {
                let Some(name) = self.builds_recipe() else {
                    return Vec::new();
                };
                let Some(resource) = recipe::named(&name).and_then(|r| r.resource.clone()) else {
                    self.set_status(format!("{} names no resource to hold", name));
                    return Vec::new();
                };
                let held = self
                    .builds
                    .holds
                    .get(&resource)
                    .is_some_and(Option::is_some);
                if !held {
                    match hold::ensure(&resource) {
                        Ok(h) => {
                            self.builds.holds.insert(resource.clone(), Some(h));
                            self.set_status(format!("reserving {}", resource));
                        }
                        Err(e) => self.set_status(format!("reserve {} failed: {}", resource, e)),
                    }
                    return Vec::new();
                }
                // The broker is a Python process: off the event loop.
                self.set_status(format!("releasing {}", resource));
                self.builds.holds.insert(resource.clone(), None);
                std::thread::spawn(move || {
                    if let Err(e) = hold::release(&resource) {
                        log::warn!("builds: release {}: {}", resource, e);
                    }
                });
            }
            OpenLog => {
                let Some(name) = self.builds_recipe() else {
                    return Vec::new();
                };
                match self.builds.shown(&name).map(|b| b.id.clone()) {
                    Some(id) => {
                        self.builds.log = Some(LogView::open(&id));
                        self.view = View::BuildLog;
                    }
                    None => self.set_status(format!("{}: nothing built yet", name)),
                }
            }
            CloseLog => {
                self.builds.log = None;
                self.view = View::Grid;
            }
            LogUp | LogDown | LogPageUp | LogPageDown | LogEnd => {
                if let Some(log) = self.builds.log.as_mut() {
                    match cmd {
                        LogUp => log.scroll(-1),
                        LogDown => log.scroll(1),
                        LogPageUp => log.scroll(-20),
                        LogPageDown => log.scroll(20),
                        _ => log.back = 0,
                    }
                }
            }
        }
        Vec::new()
    }

    /// The selected recipe card's name, or why there is none.
    fn builds_recipe(&mut self) -> Option<String> {
        let name = self.builds.selected_recipe().map(str::to_string);
        if name.is_none() {
            self.set_status("no [builds.recipes] in config.toml".into());
        }
        name
    }

    /// Put a started build first on its recipe's card, or say why it did not
    /// start. True when it started.
    fn builds_start(&mut self, started: std::io::Result<crate::builds::Build>) -> bool {
        match started {
            Ok(build) => {
                self.set_status(format!("queued {} ({})", build.target(), build.recipe));
                self.builds.select_recipe(&build.recipe);
                self.builds.builds.insert(0, build);
                true
            }
            Err(e) => {
                self.set_status(format!("build refused: {}", e));
                false
            }
        }
    }
}
