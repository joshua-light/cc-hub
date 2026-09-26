//! Linux: everything comes from procfs.

use super::ProcessInfo;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

pub struct Process;

fn stat_fields(pid: u32) -> Option<Vec<String>> {
    let stat = fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    let after_comm = stat.rfind(')')? + 2;
    Some(
        stat[after_comm..]
            .split_whitespace()
            .map(String::from)
            .collect(),
    )
}

impl ProcessInfo for Process {
    fn parent_pid(pid: u32) -> Option<u32> {
        stat_fields(pid)?.get(1)?.parse().ok()
    }

    fn name(pid: u32) -> String {
        fs::read_to_string(format!("/proc/{}/comm", pid))
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    fn session_id(pid: u32) -> Option<u32> {
        stat_fields(pid)?.get(3)?.parse().ok()
    }

    fn is_claude(pid: u32) -> bool {
        <Self as ProcessInfo>::name(pid) == "claude"
    }

    fn is_alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
}

pub fn command_line(pid: u32) -> String {
    std::fs::read(format!("/proc/{}/cmdline", pid))
        .ok()
        .map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .replace('\0', " ")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

pub fn current_dir(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{}/cwd", pid))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

pub fn list_pids() -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .collect()
}

/// Resolve candidate Codex rollout files to the live processes that currently
/// hold them open. Codex keeps its active rollout descriptor open for the
/// lifetime of an interactive session, making this stronger than cwd/mtime
/// inference when several sessions run in one project.
pub fn open_codex_rollouts(pids: &[u32], candidates: &[PathBuf]) -> HashMap<u32, PathBuf> {
    let wanted: HashSet<&PathBuf> = candidates.iter().collect();
    let mut out = HashMap::new();
    for &pid in pids {
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(path) = std::fs::read_link(fd.path()) else {
                continue;
            };
            if wanted.contains(&path) {
                out.insert(pid, path);
                break;
            }
        }
    }
    out
}
