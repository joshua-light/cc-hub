//! Spawn agent sessions backed by detached multiplexer sessions.
//!
//! Detachment lets the hub inject prompts via `send-keys` without stealing
//! focus, and the agent survives an accidentally-closed terminal. Users
//! attach on demand via the hub UI.

use crate::agent::{AgentConfig, AgentKind};
use crate::config;
use crate::pi_bridge;
use crate::platform::mux;
use crate::platform::paths;
#[cfg(not(windows))]
use crate::platform::terminal;
use crate::platform::terminal::shell_quote;
#[cfg(not(windows))]
use log::info;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub enum ResumeTarget {
    SessionId(String),
    SessionFile(PathBuf),
}

pub fn spawn_agent_session(
    agent_id: &str,
    cwd: &str,
    resume: Option<ResumeTarget>,
    initial_prompt: Option<&str>,
    readonly_tools: bool,
) -> io::Result<String> {
    let agent = config::get()
        .agent(agent_id)
        .ok_or_else(|| io::Error::other(format!("unknown agent id: {}", agent_id)))?;
    spawn_agent_session_with_config(&agent, cwd, resume, initial_prompt, readonly_tools)
}

pub fn spawn_claude_session(cwd: &str, resume_id: Option<&str>) -> io::Result<String> {
    spawn_agent_session(
        "claude",
        cwd,
        resume_id.map(|sid| ResumeTarget::SessionId(sid.to_string())),
        None,
        false,
    )
}

pub fn spawn_agent_session_with_config(
    agent: &AgentConfig,
    cwd: &str,
    resume: Option<ResumeTarget>,
    initial_prompt: Option<&str>,
    readonly_tools: bool,
) -> io::Result<String> {
    let name = unique_session_name("cchub");
    let cmd = build_agent_command(agent, cwd, &name, resume, initial_prompt, readonly_tools)?;
    if agent.kind == AgentKind::Claude {
        ensure_path_trusted(cwd)?;
    }
    mux::spawn_detached(&name, cwd, Some(&cmd))?;
    Ok(name)
}

/// Prefix `cmd` with an environment-variable assignment the target shell can
/// parse. On Unix the command runs through a POSIX shell (`$SHELL -ic`, see
/// [`mux::spawn_detached`]); on Windows it lands in pwsh. `VAR=value cmd` is
/// POSIX-only — pwsh reads it as an argument, not an assignment, so the agent
/// never starts.
///
/// Unix keeps the exact `VAR='value' cmd` form (POSIX single-quote escaping via
/// [`shell_quote`]); Windows emits `$env:VAR = 'value'; cmd` with pwsh
/// single-quote-literal escaping (double any embedded single quote). Pure and
/// `windows`-parameterised so both branches are unit-testable on any host — we
/// can't run pwsh in CI.
fn prefix_env(var: &str, value: &str, cmd: &str, windows: bool) -> String {
    if windows {
        let escaped = value.replace('\'', "''");
        format!("$env:{} = '{}'; {}", var, escaped, cmd)
    } else {
        format!("{}={} {}", var, shell_quote(value), cmd)
    }
}

fn build_agent_command(
    agent: &AgentConfig,
    cwd: &str,
    tmux_name: &str,
    resume: Option<ResumeTarget>,
    initial_prompt: Option<&str>,
    readonly_tools: bool,
) -> io::Result<String> {
    let mut cmd = agent.command.clone();

    match agent.kind {
        AgentKind::Claude => {
            match resume {
                Some(ResumeTarget::SessionId(sid)) => {
                    cmd.push_str(" --resume ");
                    cmd.push_str(&shell_quote(&sid));
                }
                Some(ResumeTarget::SessionFile(path)) => {
                    return Err(io::Error::other(format!(
                        "claude backend cannot resume by session file: {}",
                        path.display()
                    )));
                }
                None => {}
            }
            // Pin the spawned `claude` to this instance's account. The hub
            // process already has CLAUDE_CONFIG_DIR set, but a detached mux
            // session attaches to a possibly-pre-existing tmux server whose
            // captured environment may not include it — so set it explicitly.
            if let Some(dir) = paths::claude_config_dir() {
                cmd = prefix_env("CLAUDE_CONFIG_DIR", &dir.to_string_lossy(), &cmd, cfg!(windows));
            }
        }
        AgentKind::Pi => {
            if agent.use_bridge {
                let bridge = pi_bridge::ensure_bridge_file()?;
                cmd.push_str(" -e ");
                cmd.push_str(&shell_quote(&bridge.to_string_lossy()));
            }
            if readonly_tools {
                cmd.push_str(" --tools read,grep,find,ls");
            }
            match resume {
                Some(ResumeTarget::SessionId(sid)) => {
                    cmd.push_str(" --session ");
                    cmd.push_str(&shell_quote(&sid));
                }
                Some(ResumeTarget::SessionFile(path)) => {
                    cmd.push_str(" --session ");
                    cmd.push_str(&shell_quote(&path.to_string_lossy()));
                }
                None => {}
            }
            if let Some(prompt) = initial_prompt {
                cmd.push(' ');
                cmd.push_str(&shell_quote(prompt));
            }
            if agent.use_bridge {
                let heartbeat_dir = paths::pi_heartbeats_dir()
                    .ok_or_else(|| io::Error::other("home dir unavailable for pi heartbeat dir"))?;
                std::fs::create_dir_all(&heartbeat_dir)?;
                cmd = format!(
                    "CC_HUB_TMUX={} CC_HUB_AGENT_ID={} CC_HUB_HEARTBEAT_DIR={} {}",
                    shell_quote(tmux_name),
                    shell_quote(&agent.id),
                    shell_quote(&heartbeat_dir.to_string_lossy()),
                    cmd
                );
            }
        }
    }

    let _ = cwd;
    Ok(cmd)
}

fn ensure_path_trusted(cwd: &str) -> io::Result<()> {
    // Must target the SAME `.claude.json` the spawned `claude` will read — i.e.
    // the one under CLAUDE_CONFIG_DIR when set, not always `~/.claude.json`.
    // Marking the wrong file leaves the real config untrusted, so claude blocks
    // on its trust dialog at startup and never writes a session status file —
    // making the session invisible to the hub watching that config dir.
    let Some(config_path) = paths::claude_config_json() else {
        return Ok(());
    };
    // Nothing to trust-mark until claude has written its config at least once
    // (the original read short-circuits on NotFound). Bailing here also avoids
    // creating a sidecar lock in a config dir that may not exist yet.
    if !config_path.exists() {
        return Ok(());
    }
    // Serialize cc-hub's own read-modify-write of this account-wide file so a
    // concurrent spawn (e.g. an auto_review tick racing a keypress spawn) can't
    // clobber another project's freshly written trust entry via last-writer-
    // wins. The lock lives in a sidecar file because the store is tempfile+
    // rename — flock follows the inode, so the target itself can't be locked
    // across a replace. Residual limitation: the running `claude` process does
    // NOT take this lock, so it only prevents cc-hub-vs-cc-hub lost updates and
    // narrows — does not close — the window against an external writer.
    use fs2::FileExt;
    let mut lock_path = config_path.clone().into_os_string();
    lock_path.push(".cc-hub.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(PathBuf::from(lock_path))?;
    lock.lock_exclusive()?;
    // Read AFTER taking the lock so the mutation applies to fresh state, not a
    // snapshot another cc-hub writer has since superseded. `lock` stays live
    // to the end of the function; Drop releases the flock after the rename.
    let canon = std::fs::canonicalize(cwd).unwrap_or_else(|_| Path::new(cwd).to_path_buf());
    let data = match std::fs::read_to_string(&config_path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut root: serde_json::Value = serde_json::from_str(&data)
        .map_err(|e| io::Error::other(format!("parse {}: {}", config_path.display(), e)))?;
    let root_obj = root.as_object_mut().ok_or_else(|| {
        io::Error::other(format!("{} root is not an object", config_path.display()))
    })?;
    let projects = root_obj
        .entry("projects".to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            io::Error::other(format!(
                "{} projects is not an object",
                config_path.display()
            ))
        })?;
    let path_key = canon.to_string_lossy().into_owned();
    let project = projects
        .entry(path_key)
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            io::Error::other(format!(
                "{} project entry is not an object",
                config_path.display()
            ))
        })?;
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
        .map_err(|e| io::Error::other(format!("serialize {}: {}", config_path.display(), e)))?;
    let tmp = config_path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &config_path)?;
    log::info!(
        "marked {} trusted in {} before spawning claude",
        canon.display(),
        config_path.display()
    );
    Ok(())
}

pub fn spawn_shell_tmux_session(cwd: &str) -> io::Result<String> {
    let name = unique_session_name("cchub-sh");
    mux::spawn_detached(&name, cwd, None)?;
    Ok(name)
}

pub fn spawn_log_viewer_tmux_session(log_path: &Path) -> io::Result<String> {
    let name = unique_session_name("cchub-log");
    let cwd = log_path.parent().and_then(|p| p.to_str()).unwrap_or(".");
    let quoted = format!("'{}'", log_path.to_string_lossy().replace('\'', "'\\''"));
    let cmd = format!("less +G -R -- {}", quoted);
    mux::spawn_detached(&name, cwd, Some(&cmd))?;
    Ok(name)
}

#[cfg(not(windows))]
pub fn attach_tmux_session(tmux_name: &str, cwd: &str) -> io::Result<String> {
    let launcher = terminal::pick().ok_or_else(|| {
        io::Error::other(
            "no terminal emulator found (set $TERMINAL or install kitty/foot/alacritty)",
        )
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

#[cfg(test)]
mod tests {
    use super::{prefix_env, shell_quote};

    #[test]
    fn prefix_env_unix_is_posix_and_byte_identical() {
        // Must match the pre-fix `VAR='value' cmd` form exactly.
        assert_eq!(
            prefix_env("CLAUDE_CONFIG_DIR", "/home/u/.claude", "claude --resume x", false),
            format!("CLAUDE_CONFIG_DIR={} claude --resume x", shell_quote("/home/u/.claude")),
        );
        // A value with a single quote gets POSIX close/escape/reopen quoting.
        assert_eq!(
            prefix_env("V", "a'b c", "cmd", false),
            format!("V={} cmd", shell_quote("a'b c")),
        );
    }

    #[test]
    fn prefix_env_windows_is_pwsh() {
        assert_eq!(
            prefix_env("CLAUDE_CONFIG_DIR", r"C:\Users\u\.claude", "claude --resume x", true),
            r"$env:CLAUDE_CONFIG_DIR = 'C:\Users\u\.claude'; claude --resume x",
        );
        // pwsh single-quote literal: an embedded single quote is doubled.
        assert_eq!(prefix_env("V", "a'b", "cmd", true), "$env:V = 'a''b'; cmd");
    }
}
