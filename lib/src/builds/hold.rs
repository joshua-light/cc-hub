//! The hold: a recipe's resource, taken for the Builds tab and kept until the
//! user lets it go.
//!
//! A build must not swap the player under a test somebody is running, and a
//! session must not start a test on a player a build is replacing, so both
//! claim the resource through the broker. Builds come in bursts, and handing
//! the resource back between two of them would let a session in between, so
//! the first build claims and the claim outlives it. Space on the tab (or
//! `cc-hub build reserve` / `release`) takes it ahead of any build and lets
//! it go.
//!
//! The broker ties a guest's claim to a live process, so the hold is one:
//! `cc-hub build _hold <resource>`, detached, claiming as [`GUEST`] and then
//! doing nothing but living. Its file under `~/.cc-hub/builds/holds/` says
//! where it stands, for the runners that wait on it and the tab that shows it.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// The name the hold claims under, as `cc-hub resource list` shows it.
pub const GUEST: &str = "Builds";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hold {
    pub resource: String,
    pub pid: u32,
    pub since: i64,
    /// The broker granted it. Until then the hold is waiting in its queue.
    pub granted: bool,
    /// Who has the resource while the hold waits.
    #[serde(default)]
    pub behind: Option<String>,
}

fn path(resource: &str) -> io::Result<PathBuf> {
    super::builds_dir()
        .map(|d| d.join("holds").join(format!("{}.json", resource)))
        .ok_or_else(|| io::Error::other("no home dir"))
}

/// The hold on `resource`, if its process is alive.
pub fn read(resource: &str) -> Option<Hold> {
    let raw = fs::read_to_string(path(resource).ok()?).ok()?;
    let hold: Hold = serde_json::from_str(&raw).ok()?;
    super::alive(hold.pid).then_some(hold)
}

fn write(hold: &Hold) -> io::Result<()> {
    crate::persist::save_json(&path(&hold.resource)?, hold)
}

/// Make sure something holds, or is queued to hold, `resource` for the tab.
/// Serialized by a lock so two runners starting at once start one hold.
pub fn ensure(resource: &str) -> io::Result<Hold> {
    use fs2::FileExt;
    let lock_path = path(resource)?.with_extension("lock");
    if let Some(dir) = lock_path.parent() {
        fs::create_dir_all(dir)?;
    }
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;
    lock.lock_exclusive()?;
    if let Some(hold) = read(resource) {
        return Ok(hold);
    }
    let pid = super::detach(&["build", "_hold", resource])?;
    let hold = Hold {
        resource: resource.to_string(),
        pid,
        since: super::now(),
        granted: false,
        behind: None,
    };
    write(&hold)?;
    Ok(hold)
}

/// Let `resource` go: tell the broker, then end the process the claim is
/// tied to, which would end it on its own. False when nothing was held.
pub fn release(resource: &str) -> io::Result<bool> {
    let Some(hold) = read(resource) else {
        let _ = fs::remove_file(path(resource)?);
        return Ok(false);
    };
    let pid = hold.pid.to_string();
    if let Err(e) = broker(&["release", resource, "--pid", &pid]) {
        log::warn!("builds: release {}: {}", resource, e);
    }
    crate::platform::process::terminate(hold.pid);
    fs::remove_file(path(resource)?)?;
    Ok(true)
}

/// The hold's own process: claim as [`GUEST`] until granted, then live.
pub fn keep(resource: &str) -> io::Result<()> {
    let me = std::process::id();
    let pid = me.to_string();
    let mut hold = read(resource)
        .filter(|h| h.pid == me)
        .unwrap_or_else(|| Hold {
            resource: resource.to_string(),
            pid: me,
            since: super::now(),
            granted: false,
            behind: None,
        });
    // The first ask returns at once, so a hold that has to queue says behind
    // whom straight away; later ones wait in the broker between looks.
    let mut wait = "0";
    loop {
        let answer = broker(&[
            "claim", resource, "--as", GUEST, "--pid", &pid, "--wait", wait,
        ]);
        wait = "30";
        match answer {
            Ok(result) if result["ok"].as_bool() == Some(true) => {
                hold.granted = true;
                hold.behind = None;
                hold.since = super::now();
                write(&hold)?;
                break;
            }
            Ok(result) => {
                hold.behind = result["queue"][resource]["held_by"]
                    .as_str()
                    .map(str::to_string);
                write(&hold)?;
            }
            Err(e) => {
                // An unknown resource or a broken broker will not fix itself;
                // a hold that never comes would leave its builds queued forever.
                fs::remove_file(path(resource)?)?;
                return Err(e);
            }
        }
    }
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// Who has `resource` right now, by the broker's account.
pub fn holder(resource: &str) -> Option<String> {
    let result = broker(&["list"]).ok()?;
    result[resource]["holder"].as_str().map(str::to_string)
}

/// Whether `resources.toml` has `resource` at all.
pub fn known(resource: &str) -> io::Result<bool> {
    Ok(broker(&["list"])?.get(resource).is_some())
}

/// `cc-hub resource <args>`, answered by its `result`.
fn broker(args: &[&str]) -> io::Result<serde_json::Value> {
    let output = Command::new(std::env::current_exe()?)
        .arg("resource")
        .args(args)
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().last().unwrap_or_default();
    let mut answer: serde_json::Value = serde_json::from_str(line)
        .map_err(|_| io::Error::other(format!("broker said {:?}", stdout.trim())))?;
    if answer["ok"].as_bool() != Some(true) {
        let error = answer["error"].as_str().unwrap_or("resource broker failed");
        return Err(io::Error::other(error.to_string()));
    }
    Ok(answer["result"].take())
}
