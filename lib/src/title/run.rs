use super::resolve::spawn_argv;
use super::scratch_cwd;
use crate::config;
use log::{debug, warn};
use std::fs;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Signal all in-flight title subprocesses to kill their children and
/// return, so quitting the app doesn't block on up to ~45s of pending
/// Haiku calls. Call this once from the TUI just before cleanup.
pub fn request_shutdown() {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

/// Whether [`request_shutdown`] has been called. Subprocess loops poll it
/// so a quit doesn't wait on an in-flight child.
pub(crate) fn shutting_down() -> bool {
    SHUTDOWN.load(Ordering::Relaxed)
}

/// Put the child in its own session so it can't touch our controlling
/// terminal. An interactive zsh left in our session calls `tcsetpgrp` on
/// `/dev/tty` as part of its job-control setup — with the TUI owning that
/// same tty, the parent's raw-mode / alt-screen state ends up scrambled.
/// `setsid` both gives the child a fresh process group and detaches it
/// from any controlling terminal; a later `open("/dev/tty")` then fails
/// cleanly instead of hijacking ours.
#[cfg(unix)]
pub(crate) fn detach_from_tty(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
pub(crate) fn detach_from_tty(_cmd: &mut Command) {}

/// Run `cmd` detached from our tty, killing it past `timeout` or on
/// [`request_shutdown`]. `None` on spawn failure, timeout or shutdown. The
/// caller configures stdio. Shared with the persistent-agent harness, which
/// drives the same spawn command in `-p` mode.
pub fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Option<Output> {
    detach_from_tty(&mut cmd);
    let mut child: Child = cmd
        .spawn()
        .map_err(|e| warn!("title: spawn failed: {}", e))
        .ok()?;

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if shutting_down() {
                    debug!("title: shutdown signal, killing subprocess");
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                if std::time::Instant::now() >= deadline {
                    warn!("title: subprocess timed out after {:?}, killing", timeout);
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                // Short poll so a quit that lands mid-sleep adds at most
                // 100ms of quit latency per in-flight title.
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                warn!("title: try_wait failed: {}", e);
                return None;
            }
        }
    }
    child.wait_with_output().ok()
}

/// Run `<spawn.command> --model <model> -p <prompt>` in the scratch cwd
/// and return the raw stdout. Resolves the configured spawn command
/// through the user's login shell on first call (cached afterwards), then
/// execs the resolved binary directly. Returns `None` on any failure
/// (resolve, spawn, non-zero exit, timeout, shutdown).
pub fn run_claude_blocking(model: &str, prompt: &str, timeout: Duration) -> Option<String> {
    fs::create_dir_all(scratch_cwd()).ok()?;
    let resolved = spawn_argv()?;
    let (exe, base_args) = resolved.split_first()?;
    let mut cmd = Command::new(exe);
    cmd.args(base_args)
        .arg("--model")
        .arg(model)
        .arg("-p")
        .arg(prompt)
        .current_dir(scratch_cwd())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    debug!(
        "claude_blocking: model={} prompt_len={} timeout={:?}",
        model,
        prompt.len(),
        timeout
    );

    let output = run_with_timeout(cmd, timeout)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            "claude_blocking: {} exit={} stderr={:?}",
            config::get().spawn.command,
            output.status,
            stderr.trim()
        );
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}
