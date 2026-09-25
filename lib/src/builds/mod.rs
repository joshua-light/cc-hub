//! Builds (the Builds tab): one run of a [`Recipe`] over a checkout, from the
//! moment it is asked for until it succeeds, fails or is cancelled.
//!
//! Each build is a directory under `~/.cc-hub/builds/<id>/`, `build.json`
//! beside `output.log`, so the TUI, `cc-hub build` and the runner doing the
//! work read one record. Only the [`runner`] changes a build by doing
//! something: it waits its turn, gets the recipe's resource [`hold`], runs the
//! recipe and folds what the recipe [`Report`]s into the record. Everybody else
//! asks: a cancel is a flag the runner acts on, a serve is a runner of its own.

pub mod hold;
pub mod recipe;
pub mod runner;

pub use recipe::Recipe;

use crate::platform::paths::cc_hub_home;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildStatus {
    /// Waiting for an earlier build of its recipe, or for the resource.
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl BuildStatus {
    pub fn is_finished(self) -> bool {
        matches!(
            self,
            BuildStatus::Succeeded | BuildStatus::Failed | BuildStatus::Cancelled
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            BuildStatus::Queued => "queued",
            BuildStatus::Running => "running",
            BuildStatus::Succeeded => "succeeded",
            BuildStatus::Failed => "failed",
            BuildStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Build {
    pub id: String,
    pub recipe: String,
    pub cwd: String,
    /// What to build. `None` is the working tree as it stands when the build
    /// starts, not when it was queued.
    pub r#ref: Option<String>,
    /// The route asked for. `None` lets the recipe choose.
    pub route: Option<String>,
    /// Serve it the moment it is built.
    pub serve: bool,
    pub status: BuildStatus,
    pub created_at: i64,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// The subject of the commit the build started from: the ref's, or
    /// `HEAD`'s under a working tree.
    #[serde(default)]
    pub subject: Option<String>,
    /// What the recipe reported: the commit it built, the route it took, and
    /// what it is doing now.
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub taken: Option<String>,
    #[serde(default)]
    pub phase: Option<String>,
    /// The runner's pid while it lives; a build whose runner is gone without
    /// finishing it is failed by whoever reads it next.
    #[serde(default)]
    pub runner: Option<u32>,
    #[serde(default)]
    pub cancel: bool,
    #[serde(default)]
    pub served_at: Option<i64>,
    /// Why it failed, when that is not the command's exit code.
    #[serde(default)]
    pub error: Option<String>,
}

impl Build {
    pub fn new(
        recipe: &str,
        cwd: &str,
        r#ref: Option<String>,
        route: Option<String>,
        serve: bool,
    ) -> Self {
        Self {
            id: new_id(),
            recipe: recipe.to_string(),
            cwd: cwd.to_string(),
            r#ref,
            route,
            serve,
            status: BuildStatus::Queued,
            created_at: now(),
            started_at: None,
            finished_at: None,
            exit_code: None,
            subject: None,
            commit: None,
            taken: None,
            phase: None,
            runner: None,
            cancel: false,
            served_at: None,
            error: None,
        }
    }

    /// This build's checkout as it is now: the same checkout and serve, but
    /// the working tree rather than the ref, and whatever route the recipe
    /// picks for what changed. A pinned ref or route is a new build, not a
    /// rebuild.
    pub fn again(&self) -> Build {
        Build::new(&self.recipe, &self.cwd, None, None, self.serve)
    }

    /// What the card calls it: the ref, or the working tree.
    pub fn target(&self) -> &str {
        self.r#ref.as_deref().unwrap_or("working tree")
    }

    /// Seconds spent running: so far, or in total once finished.
    pub fn elapsed(&self, now: i64) -> Option<i64> {
        let start = self.started_at?;
        Some((self.finished_at.unwrap_or(now) - start).max(0))
    }

    fn finish(&mut self, status: BuildStatus, error: Option<String>) {
        self.status = status;
        self.finished_at = Some(now());
        self.runner = None;
        self.error = error.or(self.error.take());
    }
}

/// A line of recipe output addressed to the hub rather than to a person:
/// `cc-hub: <key> <value>`. The runner exports `CC_HUB_BUILD`, so a recipe
/// that also runs by hand can stay quiet when nobody is listening.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// The commit being built: known once the recipe resolved the ref or
    /// snapshotted the working tree.
    Commit(String),
    /// The route the recipe took.
    Route(String),
    /// What it is doing now, in a few words.
    Phase(String),
}

impl Report {
    pub fn parse(line: &str) -> Option<Report> {
        let (key, value) = line.strip_prefix("cc-hub: ")?.split_once(' ')?;
        let value = value.trim().to_string();
        if value.is_empty() {
            return None;
        }
        match key {
            "commit" => Some(Report::Commit(value)),
            "route" => Some(Report::Route(value)),
            "phase" => Some(Report::Phase(value)),
            _ => None,
        }
    }

    fn apply(self, build: &mut Build) {
        match self {
            Report::Commit(c) => build.commit = Some(c),
            Report::Route(r) => build.taken = Some(r),
            Report::Phase(p) => build.phase = Some(p),
        }
    }
}

// ---- the store --------------------------------------------------------------

pub fn builds_dir() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("builds"))
}

fn build_dir(id: &str) -> io::Result<PathBuf> {
    builds_dir()
        .map(|d| d.join(id))
        .ok_or_else(|| io::Error::other("no home dir"))
}

pub fn output_path(id: &str) -> io::Result<PathBuf> {
    Ok(build_dir(id)?.join("output.log"))
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `bd-<unix-nanos>`: sortable, like the board's `tk-` ids.
fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("bd-{}", nanos)
}

pub fn create(build: &Build) -> io::Result<()> {
    fs::create_dir_all(build_dir(&build.id)?)?;
    write(build)
}

fn write(build: &Build) -> io::Result<()> {
    crate::persist::save_json(&build_dir(&build.id)?.join("build.json"), build)
}

pub fn load(id: &str) -> io::Result<Build> {
    let path = build_dir(id)?.join("build.json");
    let raw = fs::read_to_string(&path)?;
    serde_json::from_str(&raw).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {}", path.display(), e),
        )
    })
}

/// Read, mutate and write one build under its lock, so the runner's reports
/// and a cancel from the TUI never lose each other.
pub fn update<F: FnOnce(&mut Build)>(id: &str, f: F) -> io::Result<Build> {
    use fs2::FileExt;
    let dir = build_dir(id)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("build.lock"))?;
    lock.lock_exclusive()?;
    let mut build = load(id)?;
    f(&mut build);
    write(&build)?;
    Ok(build)
}

pub fn append_output(id: &str, text: &str) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(output_path(id)?)?;
    writeln!(file, "{}", text)
}

/// Every build, newest first. A build left unfinished by a runner that is no
/// longer alive is failed here, by its first reader, so a crash never leaves
/// a card spinning.
pub fn all() -> Vec<Build> {
    let Some(dir) = builds_dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut builds: Vec<Build> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|name| name.starts_with("bd-"))
        .filter_map(|id| match load(&id) {
            Ok(b) => Some(b),
            Err(e) => {
                log::warn!("builds: {}: {}", id, e);
                None
            }
        })
        .map(reap)
        .collect();
    builds.sort_by(|a, b| b.id.cmp(&a.id));
    builds
}

/// A build is still somebody's while its runner is alive. One queued without a
/// runner for a minute never got one.
fn reap(build: Build) -> Build {
    if build.status.is_finished() {
        return build;
    }
    let orphaned = match build.runner {
        Some(pid) => !alive(pid),
        None => now() - build.created_at > 60,
    };
    if !orphaned {
        return build;
    }
    let error = match build.runner {
        Some(_) => "its runner exited without finishing it",
        None => "no runner ever picked it up",
    };
    update(&build.id, |b| {
        if !b.status.is_finished() {
            b.finish(BuildStatus::Failed, Some(error.into()));
        }
    })
    .unwrap_or(build)
}

/// How many finished builds a recipe keeps: its history, which is what the
/// route timings are measured from. Older ones are removed as new ones start.
const KEPT: usize = 20;

/// Remove the finished builds of `recipe` beyond the newest [`KEPT`].
fn prune(recipe: &str) {
    let old = all()
        .into_iter()
        .filter(|b| b.recipe == recipe && b.status.is_finished())
        .skip(KEPT);
    for build in old {
        if let Err(e) = build_dir(&build.id).and_then(fs::remove_dir_all) {
            log::warn!("builds: prune {}: {}", build.id, e);
        }
    }
}

/// The usual running time of a route: the median of its last ten successful
/// builds. What a running build is measured against.
pub fn typical(builds: &[Build], recipe: &str, route: &str) -> Option<i64> {
    let mut times: Vec<i64> = builds
        .iter()
        .filter(|b| b.status == BuildStatus::Succeeded)
        .filter(|b| b.recipe == recipe && b.taken.as_deref() == Some(route))
        .filter_map(|b| b.elapsed(b.finished_at.unwrap_or(0)))
        .take(10)
        .collect();
    if times.is_empty() {
        return None;
    }
    times.sort_unstable();
    Some(times[times.len() / 2])
}

// ---- asking for things ------------------------------------------------------

/// Queue a build and start its runner.
pub fn start(build: Build) -> io::Result<Build> {
    let recipe = recipe::named(&build.recipe)
        .ok_or_else(|| io::Error::other(format!("no recipe named {:?}", build.recipe)))?;
    if let Some(route) = &build.route {
        if !recipe.routes.contains(route) {
            return Err(io::Error::other(format!(
                "{} has no route {:?}; it knows {}",
                build.recipe,
                route,
                recipe.routes.join(", ")
            )));
        }
    }
    create(&build)?;
    prune(&build.recipe);
    if let Err(e) = detach(&["build", "_run", &build.id]) {
        update(&build.id, |b| {
            b.finish(BuildStatus::Failed, Some(format!("runner: {}", e)))
        })?;
        return Err(e);
    }
    Ok(build)
}

/// The checkout of build `id`, built again as it is now ([`Build::again`]).
pub fn rebuild(id: &str) -> io::Result<Build> {
    start(load(id)?.again())
}

/// Build a recipe's own checkout as it is now, for when there is no build to
/// rebuild yet.
pub fn fresh(name: &str) -> io::Result<Build> {
    let checkout = recipe::named(name)
        .and_then(|r| r.checkout.clone())
        .ok_or_else(|| io::Error::other(format!("{} names no checkout; n picks one", name)))?;
    start(Build::new(name, &checkout, None, None, true))
}

/// Ask the runner to stop. A queued build has nothing to stop yet and is
/// cancelled here; a running one is cancelled by its runner once the recipe's
/// own cancel has run.
pub fn cancel(id: &str) -> io::Result<Build> {
    update(id, |b| {
        if b.status.is_finished() {
            return;
        }
        b.cancel = true;
        if b.status == BuildStatus::Queued {
            b.finish(BuildStatus::Cancelled, None);
        }
    })
}

/// Serve a build in the background: a runner of its own that takes the hold,
/// runs the recipe's serve and records it on the build.
pub fn serve(id: &str) -> io::Result<()> {
    let build = load(id)?;
    let recipe = recipe::named(&build.recipe)
        .ok_or_else(|| io::Error::other(format!("no recipe named {:?}", build.recipe)))?;
    if recipe.serve.is_empty() {
        return Err(io::Error::other(format!("{} cannot serve", build.recipe)));
    }
    if build.status != BuildStatus::Succeeded {
        return Err(io::Error::other(
            "only a build that succeeded can be served",
        ));
    }
    detach(&["build", "_serve", id]).map(|_| ())
}

/// Start `cc-hub <args>` as a process of its own session, so it outlives the
/// TUI or the shell that asked for it. Its pid is its only handle.
pub(crate) fn detach(args: &[&str]) -> io::Result<u32> {
    use std::process::{Command, Stdio};
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe and touches no parent state.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    let mut child = command.spawn()?;
    let pid = child.id();
    // Reaped while this process lives: a zombie still answers signal 0, and
    // an ended hold must not read as a live one.
    std::thread::spawn(move || child.wait());
    Ok(pid)
}

pub(crate) fn alive(pid: u32) -> bool {
    use crate::platform::process::{Process, ProcessInfo};
    Process::is_alive(pid)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_util::with_temp_home;

    #[test]
    fn a_report_is_a_prefixed_key_and_value() {
        assert_eq!(
            Report::parse("cc-hub: route swap"),
            Some(Report::Route("swap".into()))
        );
        assert_eq!(
            Report::parse("cc-hub: phase compiling scripts"),
            Some(Report::Phase("compiling scripts".into()))
        );
        assert_eq!(Report::parse("build-server: swap build of 1a2b3c4"), None);
        assert_eq!(Report::parse("cc-hub: colour blue"), None);
        assert_eq!(Report::parse("cc-hub: commit "), None);
    }

    #[test]
    fn a_runner_that_died_fails_its_build() {
        with_temp_home(|| {
            let mut build = Build::new("build-server", "/tmp", None, None, false);
            build.runner = Some(i32::MAX as u32);
            create(&build).unwrap();
            let read = all();
            assert_eq!(read[0].status, BuildStatus::Failed);
            assert!(read[0].error.as_deref().unwrap().contains("runner exited"));
        });
    }

    #[test]
    fn a_rebuild_takes_the_checkout_as_it_is_now() {
        let pinned = Build::new(
            "build-server",
            "/repo",
            Some("1a2b3c4".into()),
            Some("full".into()),
            true,
        );
        let again = pinned.again();
        assert_eq!((again.r#ref, again.route), (None, None));
        assert_eq!((again.cwd.as_str(), again.serve), ("/repo", true));
        assert_ne!(again.id, pinned.id);
    }

    #[test]
    fn a_recipe_keeps_its_newest_finished_builds() {
        with_temp_home(|| {
            let mut ids = Vec::new();
            for n in 0..KEPT + 3 {
                let mut b = Build::new("build-server", "/tmp", None, None, false);
                b.id = format!("bd-{:04}", n);
                b.status = BuildStatus::Succeeded;
                create(&b).unwrap();
                ids.push(b.id);
            }
            let mut other = Build::new("other", "/tmp", None, None, false);
            other.id = "bd-0000-other".into();
            other.status = BuildStatus::Failed;
            create(&other).unwrap();

            prune("build-server");
            let left: Vec<String> = all().into_iter().map(|b| b.id).collect();
            assert_eq!(left.len(), KEPT + 1);
            assert!(!left.contains(&ids[0]) && !left.contains(&ids[2]));
            assert!(left.contains(&ids[3]) && left.contains(&other.id));
        });
    }

    #[test]
    fn a_queued_build_is_cancelled_on_the_spot() {
        with_temp_home(|| {
            let build = Build::new("build-server", "/tmp", None, None, false);
            create(&build).unwrap();
            let cancelled = cancel(&build.id).unwrap();
            assert_eq!(cancelled.status, BuildStatus::Cancelled);
            assert!(cancelled.finished_at.is_some());
        });
    }

    #[test]
    fn the_typical_time_is_the_median_of_the_route() {
        let run = |route: &str, secs: i64, status| {
            let mut b = Build::new("build-server", "/tmp", None, None, false);
            b.taken = Some(route.into());
            b.status = status;
            b.started_at = Some(0);
            b.finished_at = Some(secs);
            b
        };
        let builds = vec![
            run("swap", 60, BuildStatus::Succeeded),
            run("swap", 90, BuildStatus::Succeeded),
            run("swap", 5, BuildStatus::Failed),
            run("swap", 70, BuildStatus::Succeeded),
            run("full", 1200, BuildStatus::Succeeded),
        ];
        assert_eq!(typical(&builds, "build-server", "swap"), Some(70));
        assert_eq!(typical(&builds, "build-server", "scripts"), None);
    }
}
