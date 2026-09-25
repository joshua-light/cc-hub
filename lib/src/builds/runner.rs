//! The runner: `cc-hub build _run <id>`, detached, the one process that moves
//! a build forward.
//!
//! It waits for its turn, since a recipe builds one thing at a time, oldest
//! first. Then it waits for the recipe's [`hold`](super::hold), runs the build
//! command and writes every line it prints to `output.log`, folding
//! [`Report`]s into the record. When asked to cancel it runs the recipe's own
//! cancel and ends the command if that did not. A build that succeeded and
//! asked to be served is served by the same runner, before it exits.

use super::recipe::{self, Recipe, Values};
use super::{append_output, hold, load, update, Build, BuildStatus, Report};
use std::io::{self, BufRead, BufReader};
use std::process::{Child, ExitStatus};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// How long a cancelled command gets to end on its own, after the recipe's
/// cancel, before it is ended.
const CANCEL_GRACE: Duration = Duration::from_secs(15);

pub fn run(id: &str) -> io::Result<BuildStatus> {
    update(id, |b| b.runner = Some(std::process::id()))?;
    // Whatever stops the runner stops the build with it, in its own words.
    drive(id).or_else(|e| fail(id, e.to_string()))
}

fn drive(id: &str) -> io::Result<BuildStatus> {
    let build = load(id)?;
    let Some(recipe) = recipe::named(&build.recipe) else {
        return fail(
            id,
            format!("no recipe named {:?} in config.toml", build.recipe),
        );
    };

    if !wait(id, turn)? {
        return Ok(load(id)?.status);
    }
    if let Some(resource) = &recipe.resource {
        match hold::known(resource) {
            Ok(true) => {}
            Ok(false) => {
                return fail(id, format!("no resource {:?} in resources.toml", resource));
            }
            Err(e) => return fail(id, format!("resource broker: {}", e)),
        }
        let mut holds = HoldWatch::default();
        if !wait(id, |_| holds.granted(resource))? {
            return Ok(load(id)?.status);
        }
    }

    let from = build.r#ref.as_deref().unwrap_or("HEAD");
    let subject = git(&build.cwd, &["log", "-1", "--format=%s", from]);
    let build = update(id, |b| {
        b.status = BuildStatus::Running;
        b.started_at = Some(super::now());
        b.phase = None;
        b.subject = subject;
    })?;

    let argv = recipe::expand(&recipe.build, Values::of(&build));
    let (exit, last) = match stream(id, &argv, &build.cwd) {
        Ok((child, lines)) => follow(id, recipe, &build, child, lines)?,
        Err(e) => return fail(id, format!("{}: {}", argv.join(" "), e)),
    };

    let cancelled = load(id)?.cancel;
    let status = if cancelled {
        BuildStatus::Cancelled
    } else if exit.success() {
        BuildStatus::Succeeded
    } else {
        BuildStatus::Failed
    };
    let build = update(id, |b| {
        b.exit_code = exit.code();
        let error = (status == BuildStatus::Failed).then_some(last).flatten();
        b.finish(status, error);
    })?;
    if status == BuildStatus::Succeeded && build.serve && !recipe.serve.is_empty() {
        serve(id)?;
    }
    Ok(status)
}

/// Serve a build that succeeded: take the hold, run the recipe's serve, and
/// record when it worked. `cc-hub build _serve <id>` and a `serve` build's
/// runner both end here.
pub fn serve(id: &str) -> io::Result<()> {
    let build = load(id)?;
    let recipe = recipe::named(&build.recipe)
        .ok_or_else(|| io::Error::other(format!("no recipe named {:?}", build.recipe)))?;
    if let Some(resource) = &recipe.resource {
        let mut holds = HoldWatch::default();
        loop {
            match holds.granted(resource)? {
                Wait::Ready => break,
                Wait::For(why) => {
                    log::info!("serve {}: {}", id, why);
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }
    let argv = recipe::expand(&recipe.serve, Values::of(&build));
    let (mut child, lines) = stream(id, &argv, &build.cwd)?;
    for line in lines {
        append_output(id, &line)?;
    }
    if child.wait()?.success() {
        update(id, |b| b.served_at = Some(super::now()))?;
        Ok(())
    } else {
        append_output(id, "cc-hub: serve failed")?;
        Err(io::Error::other("serve failed; see the build's output"))
    }
}

/// Watch the build command: log its lines, apply its reports, and cancel it
/// when asked. Returns how it exited and the last thing it said to a person.
fn follow(
    id: &str,
    recipe: &Recipe,
    build: &Build,
    mut child: Child,
    lines: Receiver<String>,
) -> io::Result<(ExitStatus, Option<String>)> {
    let mut last = None;
    let mut cancelled_at: Option<Instant> = None;
    loop {
        match lines.recv_timeout(Duration::from_secs(1)) {
            Ok(line) => {
                append_output(id, &line)?;
                match Report::parse(&line) {
                    Some(report) => {
                        update(id, |b| report.apply(b))?;
                    }
                    None if !line.trim().is_empty() => last = Some(line),
                    None => {}
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        match cancelled_at {
            None if load(id)?.cancel => {
                cancelled_at = Some(Instant::now());
                append_output(id, "cc-hub: cancelling")?;
                if !recipe.cancel.is_empty() {
                    let argv = recipe::expand(&recipe.cancel, Values::of(build));
                    let (mut cancel, said) = stream(id, &argv, &build.cwd)?;
                    for line in said {
                        append_output(id, &line)?;
                    }
                    cancel.wait()?;
                }
            }
            Some(at) if at.elapsed() > CANCEL_GRACE => {
                crate::platform::process::terminate(child.id());
            }
            _ => {}
        }
    }
    Ok((child.wait()?, last))
}

/// `argv` started in `cwd`, its stdout and stderr as one stream of lines.
fn stream(id: &str, argv: &[String], cwd: &str) -> io::Result<(Child, Receiver<String>)> {
    if argv.is_empty() {
        return Err(io::Error::other("the recipe gives no command"));
    }
    append_output(id, &format!("$ {}", argv.join(" ")))?;
    let (reader, writer) = io::pipe()?;
    let mut command = recipe::command(argv, cwd);
    command
        .env("CC_HUB_BUILD", id)
        .stdin(std::process::Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer);
    let child = command.spawn()?;
    // The command holds the pipe's write end; while it lives the reader
    // never sees the end of the stream.
    drop(command);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    Ok((child, rx))
}

enum Wait {
    Ready,
    For(String),
}

/// Poll until `check` says ready, writing why it waits as the build's phase.
/// False when the build was cancelled while it waited.
fn wait(id: &str, mut check: impl FnMut(&Build) -> io::Result<Wait>) -> io::Result<bool> {
    loop {
        let build = load(id)?;
        if build.cancel || build.status.is_finished() {
            return Ok(false);
        }
        match check(&build)? {
            Wait::Ready => return Ok(true),
            Wait::For(why) => {
                if build.phase.as_deref() != Some(why.as_str()) {
                    update(id, |b| b.phase = Some(why))?;
                }
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// A recipe builds one thing at a time, oldest first.
fn turn(build: &Build) -> io::Result<Wait> {
    let ahead = super::all()
        .iter()
        .filter(|b| b.recipe == build.recipe && b.id < build.id && !b.status.is_finished())
        .count();
    Ok(match ahead {
        0 => Wait::Ready,
        1 => Wait::For("after 1 earlier build".into()),
        n => Wait::For(format!("after {} earlier builds", n)),
    })
}

/// Waiting for the hold, starting one when there is none. A hold that keeps
/// dying is a broker that cannot grant it, not a queue to wait in.
#[derive(Default)]
struct HoldWatch {
    pids: Vec<u32>,
}

impl HoldWatch {
    fn granted(&mut self, resource: &str) -> io::Result<Wait> {
        let hold = hold::ensure(resource)?;
        if !self.pids.contains(&hold.pid) {
            self.pids.push(hold.pid);
        }
        if self.pids.len() > 3 {
            return Err(io::Error::other(format!(
                "the hold on {} keeps ending; try `cc-hub resource claim {} --as test`",
                resource, resource
            )));
        }
        Ok(match (hold.granted, hold.behind) {
            (true, _) => Wait::Ready,
            (false, Some(who)) => Wait::For(format!("waiting for {}, held by {}", resource, who)),
            (false, None) => Wait::For(format!("waiting for {}", resource)),
        })
    }
}

fn fail(id: &str, error: String) -> io::Result<BuildStatus> {
    let _ = append_output(id, &format!("cc-hub: {}", error));
    update(id, |b| b.finish(BuildStatus::Failed, Some(error)))?;
    Ok(BuildStatus::Failed)
}

fn git(cwd: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(crate::platform::paths::expand_home(cwd))
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !text.is_empty()).then_some(text)
}
