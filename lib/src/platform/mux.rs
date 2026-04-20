//! Terminal-multiplexer CLI shim.
//!
//! cc-hub was born on tmux, but every CLI it actually uses (`new-session`,
//! `send-keys`, `list-panes`, `kill-session`) is present in psmux — a
//! tmux-compatible multiplexer that runs natively on Windows via ConPTY, and
//! ships a `tmux.exe` binary. Both platforms go through the same `tmux` CLI
//! name, and the behaviour only diverges in two places:
//!
//! - **Detached spawn with an initial command.** tmux takes the command as
//!   the trailing arg of `new-session -d -s NAME -c CWD CMD`; psmux silently
//!   drops that trailing arg. The Windows path creates a bare session and
//!   then uses `send-keys` to queue the initial command (psmux buffers the
//!   keystrokes until the child shell is ready to consume them).
//!
//! - **`list-clients -F` format strings.** tmux honours them; psmux ignores
//!   them and returns `ttyPath: SESSION: shell [WxH] (utf8)`. Only used by
//!   the focus/close paths — the Windows backend returns an empty list and
//!   those paths no-op, which is fine because the TUI embeds sessions via
//!   [`crate::tmux_pane`] rather than in separate terminal windows.

use log::{error, info, warn};
use std::io;
use std::process::Command;

/// Binary invoked for every mux operation. On Windows this resolves to
/// psmux's `tmux.exe` shim; on Unix to real tmux.
const MUX_BIN: &str = "tmux";

/// Create a detached session. When `initial_cmd` is `Some`, the session
/// runs it as its inaugural interactive command (e.g. launching `ccyo`).
pub fn spawn_detached(name: &str, cwd: &str, initial_cmd: Option<&str>) -> io::Result<()> {
    spawn_detached_impl(name, cwd, initial_cmd)?;
    enable_mouse(name);
    Ok(())
}

#[cfg(not(windows))]
fn spawn_detached_impl(name: &str, cwd: &str, initial_cmd: Option<&str>) -> io::Result<()> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    // `-ic CMD` keeps the user's shell rc in the loop (aliases, PATH) so
    // `ccyo` resolves the same way it does in an interactive terminal.
    let shell_cmd = match initial_cmd {
        Some(cmd) => format!("{} -ic {}", shell, cmd),
        None => shell,
    };
    let args = ["new-session", "-d", "-s", name, "-c", cwd, &shell_cmd];
    info!("spawn: {} {}", MUX_BIN, args.join(" "));
    run(&args, "new-session")
}

#[cfg(windows)]
fn spawn_detached_impl(name: &str, cwd: &str, initial_cmd: Option<&str>) -> io::Result<()> {
    // Two-phase: bare session, then queue the launch command via send-keys.
    // psmux buffers the keystrokes in the PTY and pwsh consumes them once
    // it's past its cold-start.
    let args = ["new-session", "-d", "-s", name, "-c", cwd];
    info!("spawn: {} {}", MUX_BIN, args.join(" "));
    run(&args, "new-session")?;
    if let Some(cmd) = initial_cmd {
        info!("spawn: queuing initial cmd in {}: {:?}", name, cmd);
        run(&["send-keys", "-t", name, "-l", cmd], "send-keys literal")?;
        run(&["send-keys", "-t", name, "Enter"], "send-keys Enter")?;
    }
    Ok(())
}

/// Type `text` into the session, then submit with Enter.
pub fn send_prompt(session: &str, text: &str) -> io::Result<()> {
    info!("send: session={} text_len={}", session, text.len());
    run(&["send-keys", "-t", session, "-l", text], "send-keys literal")?;
    run(&["send-keys", "-t", session, "Enter"], "send-keys Enter")
}

/// Snapshot of every `(pane_pid, session_name)` the server knows about.
/// Returns empty when the server is down.
pub fn list_panes() -> Vec<(u32, String)> {
    let Ok(out) = Command::new(MUX_BIN)
        .args(["list-panes", "-a", "-F", "#{pane_pid} #{session_name}"])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        warn!(
            "{} list-panes failed status={} stderr={:?}",
            MUX_BIN,
            out.status,
            stderr.trim()
        );
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let (pid_s, name) = line.split_once(' ')?;
            let pid: u32 = pid_s.parse().ok()?;
            Some((pid, name.to_string()))
        })
        .collect()
}

/// Attached-client pids for `session`. Empty on Windows: psmux ignores
/// `list-clients -F`, so we can't recover structured client info — focus
/// and close paths no-op there, since the TUI embeds sessions via
/// [`crate::tmux_pane`] rather than via separate terminal windows.
#[cfg(not(windows))]
pub fn client_pids(session: &str) -> Vec<u32> {
    let Ok(out) = Command::new(MUX_BIN)
        .args(["list-clients", "-t", session, "-F", "#{client_pid}"])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

#[cfg(windows)]
pub fn client_pids(_session: &str) -> Vec<u32> {
    Vec::new()
}

pub fn kill_session(session: &str) -> io::Result<()> {
    run(&["kill-session", "-t", session], "kill-session")
}

/// Best-effort: log and swallow failures, since a missing mouse option is
/// a degraded-experience issue, not a blocker.
pub fn enable_mouse(session: &str) {
    if let Err(e) = run(
        &["set-option", "-t", session, "mouse", "on"],
        "set-option mouse",
    ) {
        warn!("enable_mouse {}: {}", session, e);
    }
}

/// argv for attaching to `session`. Used by [`crate::tmux_pane`] when it
/// spawns a portable-pty child.
pub fn attach_argv(session: &str) -> Vec<String> {
    vec![MUX_BIN.into(), "attach".into(), "-t".into(), session.into()]
}

fn run(args: &[&str], label: &str) -> io::Result<()> {
    let out = Command::new(MUX_BIN).args(args).output()?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    error!("{} {} failed: {}", MUX_BIN, label, stderr);
    Err(io::Error::other(format!(
        "{} {} failed: {}",
        MUX_BIN,
        label,
        if stderr.is_empty() {
            out.status.to_string()
        } else {
            stderr
        }
    )))
}
