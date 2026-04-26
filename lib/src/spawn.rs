//! Spawn a new Claude session backed by a detached multiplexer session.
//!
//! Detachment lets the hub inject prompts via `send-keys` without stealing
//! focus, and the agent survives an accidentally-closed terminal. Users
//! attach on demand via the hub UI. The command run in the pane is
//! [`config::SpawnConfig::command`] — `cc-hub-new` by default.

use crate::config;
use crate::platform::mux;
#[cfg(not(windows))]
use crate::platform::{paths, terminal};
#[cfg(not(windows))]
use log::info;
use std::io;
use std::io::Write;
use std::path::Path;
#[cfg(not(windows))]
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// Spawn a new Claude session for `cwd`, returning the multiplexer session
/// name. When `resume_id` is `Some`, the pane runs `<cmd> --resume <id>`
/// instead of a fresh `<cmd>`.
pub fn spawn_claude_session(cwd: &str, resume_id: Option<&str>) -> io::Result<String> {
    let name = unique_session_name("cchub");
    let base = config::get().spawn.command.as_str();
    let cmd = match resume_id {
        Some(sid) => format!("{} --resume {}", base, sid),
        None => base.to_string(),
    };
    ensure_path_trusted(cwd)?;
    mux::spawn_detached(&name, cwd, Some(&cmd))?;
    Ok(name)
}

/// Flip `hasTrustDialogAccepted` to `true` in `~/.claude.json` for `cwd`.
/// Without this, claude blocks on a "trust this folder?" prompt the user
/// never sees in a detached pane — the agent hangs, no session metadata is
/// written, nothing surfaces in the hub.
///
/// Originally scoped to `$HOME` only (where users rarely start claude
/// manually). Generalised to any path because the orchestrator layer spawns
/// into fresh worktrees under `.cc-hub-wt/<task>-<name>` that are never
/// pre-trusted, and into ad-hoc dirs (tempdirs, new-project scaffolds).
/// The op is idempotent — paths already marked trusted return early.
fn ensure_path_trusted(cwd: &str) -> io::Result<()> {
    let Some(home) = dirs::home_dir() else {
        return Ok(());
    };
    let canon = std::fs::canonicalize(cwd).unwrap_or_else(|_| Path::new(cwd).to_path_buf());
    let config_path = home.join(".claude.json");
    let data = match std::fs::read_to_string(&config_path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut root: serde_json::Value = serde_json::from_str(&data)
        .map_err(|e| io::Error::other(format!("parse ~/.claude.json: {}", e)))?;
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| io::Error::other("~/.claude.json root is not an object"))?;
    let projects = root_obj
        .entry("projects".to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| io::Error::other("~/.claude.json projects is not an object"))?;
    let path_key = canon.to_string_lossy().into_owned();
    let project = projects
        .entry(path_key)
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| io::Error::other("~/.claude.json project entry is not an object"))?;
    if project
        .get("hasTrustDialogAccepted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Ok(());
    }
    project.insert(
        "hasTrustDialogAccepted".to_string(),
        serde_json::Value::Bool(true),
    );
    let body = serde_json::to_string_pretty(&root)
        .map_err(|e| io::Error::other(format!("serialize ~/.claude.json: {}", e)))?;
    let tmp = config_path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &config_path)?;
    log::info!(
        "marked {} trusted in ~/.claude.json before spawning claude",
        canon.display()
    );
    Ok(())
}

/// Spawn an ephemeral multiplexer session that runs an interactive shell in
/// `cwd`. Pair with [`crate::tmux_pane::TmuxPaneView::spawn_owned`] so closing
/// the popup tears the session down.
pub fn spawn_shell_tmux_session(cwd: &str) -> io::Result<String> {
    let name = unique_session_name("cchub-sh");
    mux::spawn_detached(&name, cwd, None)?;
    Ok(name)
}

/// Open a new terminal window that attaches to an existing detached session.
/// Used to recover a session whose terminal was closed — the agent kept
/// running headlessly, this brings it back into view.
///
/// Unix-only today. Windows embeds sessions inside the hub via
/// [`crate::tmux_pane`]; there's no separate terminal window to resurrect.
#[cfg(not(windows))]
pub fn attach_tmux_session(tmux_name: &str, cwd: &str) -> io::Result<String> {
    let launcher = terminal::pick().ok_or_else(|| {
        io::Error::other("no terminal emulator found (set $TERMINAL or install kitty/foot/alacritty)")
    })?;
    let attach_argv = mux::attach_argv(tmux_name);
    let attach_argv_refs: Vec<&str> = attach_argv.iter().map(|s| s.as_str()).collect();
    let argv = launcher.argv_bare(cwd, &attach_argv_refs);
    let bin = paths::terminal_wrapper_script(launcher.name())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| launcher.name().to_string());

    info!("attach: {} {}", bin, argv.join(" "));
    Command::new(&bin)
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(format!("reattached {} in {}", tmux_name, launcher.name()))
}

#[cfg(windows)]
pub fn attach_tmux_session(_tmux_name: &str, _cwd: &str) -> io::Result<String> {
    // Windows path uses the in-TUI embed (tmux_pane) exclusively.
    Err(io::Error::other(
        "attach_tmux_session: not implemented on Windows (embed via TmuxPaneView instead)",
    ))
}

fn unique_session_name(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}-{}", prefix, std::process::id(), nanos)
}
