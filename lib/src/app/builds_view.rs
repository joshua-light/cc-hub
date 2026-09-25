//! Builds-tab state: the latest on-disk snapshot of every build, what each
//! recipe's player holds and who holds each recipe's resource, the cursor over
//! the cards, and the new-build form and log view while they are open.

use crate::builds::hold::Hold;
use crate::builds::{self, Build, BuildStatus};
use std::collections::BTreeMap;

/// What the refresh reads off disk every second.
#[derive(Clone, Debug, Default)]
pub struct BuildsSnapshot {
    pub builds: Vec<Build>,
    /// The tab's hold on each recipe resource, if it has one.
    pub holds: BTreeMap<String, Option<Hold>>,
}

/// What the probe asks, less often: a process or an ssh round trip each.
#[derive(Clone, Debug, Default)]
pub struct BuildsProbe {
    /// Recipe → the commit its `current` printed.
    pub current: BTreeMap<String, String>,
    /// Resource → who the broker says holds it.
    pub holders: BTreeMap<String, Option<String>>,
}

#[derive(Default)]
pub struct BuildsView {
    pub builds: Vec<Build>,
    pub holds: BTreeMap<String, Option<Hold>>,
    pub probe: BuildsProbe,
    pub selected: usize,
    pub loaded: bool,
    pub form: Option<BuildForm>,
    pub log: Option<LogView>,
}

impl BuildsView {
    pub fn update(&mut self, snapshot: BuildsSnapshot) {
        let keep = self.selected().map(|b| b.id.clone());
        self.builds = snapshot.builds;
        self.holds = snapshot.holds;
        self.loaded = true;
        self.selected = keep
            .and_then(|id| self.builds.iter().position(|b| b.id == id))
            .unwrap_or(self.selected)
            .min(self.builds.len().saturating_sub(1));
        if let Some(log) = self.log.as_mut() {
            log.reload();
        }
    }

    pub fn selected(&self) -> Option<&Build> {
        self.builds.get(self.selected)
    }

    pub fn nav(&mut self, delta: isize) {
        if self.builds.is_empty() {
            return;
        }
        let max = self.builds.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    /// Whether this is the build its recipe's player was built from: the
    /// newest one that succeeded on the commit `current` reports.
    pub fn in_player(&self, build: &Build) -> bool {
        let Some(current) = self.probe.current.get(&build.recipe) else {
            return false;
        };
        let same = |b: &Build| {
            b.commit
                .as_deref()
                .is_some_and(|c| c.starts_with(current.as_str()) || current.starts_with(c))
        };
        self.builds
            .iter()
            .find(|b| b.recipe == build.recipe && b.status == BuildStatus::Succeeded && same(b))
            .is_some_and(|b| b.id == build.id)
    }

    pub fn typical(&self, build: &Build) -> Option<i64> {
        builds::typical(&self.builds, &build.recipe, build.taken.as_deref()?)
    }

    /// Builds that are queued or running.
    pub fn active_count(&self) -> usize {
        self.builds
            .iter()
            .filter(|b| !b.status.is_finished())
            .count()
    }
}

/// The form behind `n`: a recipe, a checkout, a ref and a route. Every field
/// starts at what a rebuild of the selected card would use, so the common
/// case is `n` and Enter.
#[derive(Clone, Debug, PartialEq)]
pub struct BuildForm {
    pub recipe: String,
    pub cwd: String,
    /// Empty is the working tree.
    pub r#ref: String,
    /// `None` lets the recipe choose.
    pub route: Option<String>,
    pub serve: bool,
    pub field: FormField,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormField {
    Recipe,
    Checkout,
    Ref,
    Route,
    Serve,
}

impl FormField {
    pub const ALL: [FormField; 5] = [
        FormField::Recipe,
        FormField::Checkout,
        FormField::Ref,
        FormField::Route,
        FormField::Serve,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FormField::Recipe => "recipe",
            FormField::Checkout => "checkout",
            FormField::Ref => "ref",
            FormField::Route => "route",
            FormField::Serve => "serve",
        }
    }

    /// Typed into, as opposed to stepped through.
    pub fn is_text(self) -> bool {
        matches!(self, FormField::Checkout | FormField::Ref)
    }
}

impl BuildForm {
    /// A form for `recipe`, seeded from `like` when there is one.
    pub fn new(recipe: &str, like: Option<&Build>) -> Self {
        let checkout = builds::recipe::named(recipe).and_then(|r| r.checkout.clone());
        match like.filter(|b| b.recipe == recipe) {
            Some(b) => Self {
                recipe: recipe.to_string(),
                cwd: b.cwd.clone(),
                r#ref: b.r#ref.clone().unwrap_or_default(),
                route: b.route.clone(),
                serve: b.serve,
                field: FormField::Ref,
            },
            None => Self {
                recipe: recipe.to_string(),
                cwd: checkout.unwrap_or_default(),
                r#ref: String::new(),
                route: None,
                serve: true,
                field: FormField::Ref,
            },
        }
    }

    pub fn step_field(&mut self, delta: isize) {
        let all = FormField::ALL;
        let i = all.iter().position(|f| *f == self.field).unwrap_or(0) as isize;
        self.field = all[(i + delta).rem_euclid(all.len() as isize) as usize];
    }

    /// Left/Right on a stepped field: the next recipe, route, or serve.
    pub fn step_value(&mut self, delta: isize) {
        match self.field {
            FormField::Recipe => {
                let names: Vec<&str> = builds::recipe::all().map(|(n, _)| n).collect();
                if let Some(i) = names.iter().position(|n| *n == self.recipe) {
                    let next =
                        names[(i as isize + delta).rem_euclid(names.len() as isize) as usize];
                    let field = self.field;
                    *self = BuildForm::new(next, None);
                    self.field = field;
                }
            }
            FormField::Route => {
                let routes = builds::recipe::named(&self.recipe)
                    .map(|r| r.routes.clone())
                    .unwrap_or_default();
                // `None` (the recipe's choice) sits before the first route.
                let choices: Vec<Option<String>> = std::iter::once(None)
                    .chain(routes.into_iter().map(Some))
                    .collect();
                let i = choices.iter().position(|c| *c == self.route).unwrap_or(0) as isize;
                self.route =
                    choices[(i + delta).rem_euclid(choices.len() as isize) as usize].clone();
            }
            FormField::Serve => self.serve = !self.serve,
            FormField::Checkout | FormField::Ref => {}
        }
    }

    pub fn text_mut(&mut self) -> Option<&mut String> {
        match self.field {
            FormField::Checkout => Some(&mut self.cwd),
            FormField::Ref => Some(&mut self.r#ref),
            _ => None,
        }
    }

    pub fn to_build(&self) -> Build {
        let r#ref = Some(self.r#ref.trim().to_string()).filter(|r| !r.is_empty());
        Build::new(
            &self.recipe,
            self.cwd.trim(),
            r#ref,
            self.route.clone(),
            self.serve,
        )
    }
}

/// The log behind Enter: `output.log`, re-read on every refresh, following
/// its end until scrolled away from it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogView {
    pub id: String,
    pub lines: Vec<String>,
    /// Lines up from the end; 0 follows the end.
    pub back: usize,
}

/// A log longer than this shows its end.
const LOG_LINES: usize = 5000;

impl LogView {
    pub fn open(id: &str) -> Self {
        let mut log = LogView {
            id: id.to_string(),
            ..LogView::default()
        };
        log.reload();
        log
    }

    pub fn reload(&mut self) {
        let text = builds::output_path(&self.id)
            .and_then(std::fs::read_to_string)
            .unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let skip = lines.len().saturating_sub(LOG_LINES);
        self.lines = lines[skip..].iter().map(|l| l.to_string()).collect();
        self.back = self.back.min(self.lines.len());
    }

    pub fn scroll(&mut self, delta: isize) {
        let back = self.back as isize - delta;
        self.back = back.clamp(0, self.lines.len() as isize) as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built(recipe: &str, commit: &str) -> Build {
        let mut b = Build::new(recipe, "/tmp", None, None, false);
        b.status = BuildStatus::Succeeded;
        b.commit = Some(commit.into());
        b
    }

    #[test]
    fn only_the_newest_build_of_the_current_commit_is_in_the_player() {
        let mut view = BuildsView::default();
        let newer = built("build-server", "1a2b3c4d5e6f708192a3b4c5d6e7f80910213243");
        let mut older = built("build-server", "1a2b3c4d5e6f708192a3b4c5d6e7f80910213243");
        older.id = "bd-1".into();
        let other = built("build-server", "9f8e7d6c5b4a39281706f5e4d3c2b1a098765432");
        view.builds = vec![newer.clone(), older.clone(), other.clone()];
        view.probe
            .current
            .insert("build-server".into(), "1a2b3c4d5e6f".into());
        assert!(view.in_player(&newer));
        assert!(!view.in_player(&older));
        assert!(!view.in_player(&other));
    }

    // Nothing here may call `BuildForm::new` or `step_value`: they read the
    // process-wide config, and the first reader fixes it for every test.
    #[test]
    fn an_empty_ref_is_the_working_tree() {
        let mut form = BuildForm {
            recipe: "build-server".into(),
            cwd: "/repo".into(),
            r#ref: "  ".into(),
            route: None,
            serve: true,
            field: FormField::Ref,
        };
        assert_eq!(form.to_build().r#ref, None);
        form.r#ref = "origin/main".into();
        assert_eq!(form.to_build().r#ref.as_deref(), Some("origin/main"));
    }

    #[test]
    fn a_log_scrolled_to_its_end_follows_it() {
        let mut log = LogView {
            id: "bd-1".into(),
            lines: (0..10).map(|i| i.to_string()).collect(),
            back: 0,
        };
        log.scroll(1);
        assert_eq!(log.back, 0);
        log.scroll(-3);
        assert_eq!(log.back, 3);
        log.scroll(-30);
        assert_eq!(log.back, 10);
    }
}
