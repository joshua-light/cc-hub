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
use std::io::{self, Write};
use std::process::{Command, Stdio};

/// Binary invoked for every mux operation. On Windows this resolves to
/// psmux's `tmux.exe` shim; on Unix to real tmux.
const MUX_BIN: &str = "tmux";

/// Create a detached session. When `initial_cmd` is `Some`, the session
/// runs it as its inaugural interactive command (e.g. launching `ccyo`).
pub fn spawn_detached(name: &str, cwd: &str, initial_cmd: Option<&str>) -> io::Result<()> {
    spawn_detached_impl(name, cwd, initial_cmd)?;
    enable_mouse(name);
    configure_clipboard();
    Ok(())
}

#[cfg(not(windows))]
fn spawn_detached_impl(name: &str, cwd: &str, initial_cmd: Option<&str>) -> io::Result<()> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    // tmux runs the trailing arg via `/bin/sh -c`, which word-splits on
    // spaces. The command must be a single quoted token for `-c`, otherwise
    // `zsh -ic cc-hub-new --resume SID` becomes `zsh -i -c cc-hub-new` with
    // `--resume SID` attached to zsh's positional params — never reaching
    // the alias. `-ic` keeps the user's shell rc in the loop (aliases, PATH).
    let shell_cmd = match initial_cmd {
        Some(cmd) => format!("{} -ic {}", shell, super::terminal::shell_quote(cmd)),
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

/// Inject `text` as a paste, then submit with Enter.
///
/// Uses [`paste_buffer`] (bracketed-paste-marked) rather than `send-keys -l`
/// so claude treats the payload as a paste — atomic from its perspective —
/// rather than a stream of typed keys. This matters in two ways:
///
/// 1. Multi-line prompts. With `-l`, each embedded newline is a literal
///    Enter to claude's input box; the first newline submits the partial
///    prompt and the rest goes to the next turn.
/// 2. Cold-session race. On a freshly-spawned session the Enter that
///    follows `-l` sometimes arrives before claude has finished setting up
///    its input handling, leaving the text typed but unsubmitted. The
///    bracketed-paste close marker (`\x1b[201~`) is a real signal claude
///    waits for before re-enabling its keystroke handler; pairing it with
///    a short pause before Enter eliminates the race observed in the
///    multi-worker orchestrator smoke test.
pub fn send_prompt(session: &str, text: &str) -> io::Result<()> {
    info!("send: session={} text_len={}", session, text.len());
    paste_buffer(session, text)?;
    // tmux's load-buffer/paste-buffer/send-keys subprocesses each fork+exec
    // independently; the close marker may still be inflight to the pane
    // when send-keys Enter starts. 80ms is well below human-perceptible and
    // 5x what we observed as the race window in practice.
    std::thread::sleep(std::time::Duration::from_millis(80));
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

/// Paste `text` into `session`'s active pane via tmux's buffer mechanism.
///
/// Wraps the payload in bracketed-paste markers (`\x1b[200~…\x1b[201~`)
/// inside the buffer itself. `paste-buffer -p` would do it automatically,
/// but only when tmux thinks the target app has DECSET 2004 enabled —
/// unreliable for the embedded-tmux case, where failure mode is "each
/// newline arrives as Enter and submits the partial line."
pub fn paste_buffer(session: &str, text: &str) -> io::Result<()> {
    // Per-call buffer name: tmux buffers are server-wide, so a fixed name
    // would race when two callers (e.g. concurrent `cc-hub spawn-worker`
    // invocations from a TUI dispatcher and a CLI subcommand) interleave
    // load-buffer/paste-buffer/-d cycles. PID + nanos is unique within one
    // host's tmux server lifetime.
    let buf_name = format!(
        "cchub-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );

    let mut payload = Vec::with_capacity(text.len() + 12);
    payload.extend_from_slice(b"\x1b[200~");
    payload.extend_from_slice(text.as_bytes());
    payload.extend_from_slice(b"\x1b[201~");

    let mut child = Command::new(MUX_BIN)
        .args(["load-buffer", "-b", &buf_name, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::other("load-buffer: no stdin"))?;
        stdin.write_all(&payload)?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(io::Error::other(format!("load-buffer failed: {}", stderr)));
    }
    // `-r` suppresses tmux's LF→CR translation; without it every embedded
    // newline arrives at the app as Enter. `-d` deletes the buffer after
    // use so we don't leak per-call buffers in `tmux list-buffers`.
    run(
        &["paste-buffer", "-b", &buf_name, "-r", "-d", "-t", session],
        "paste-buffer",
    )
}

/// Wire tmux's `copy-command` (a server option, tmux 3.2+) to the same
/// shell fallback chain cc-hub uses, so a mouse-drag selection in an
/// embedded pane lands in the host clipboard via the default
/// `copy-pipe-and-cancel` binding. Best-effort — server-wide, clobbers any
/// pre-existing `copy-command`; users who want different behaviour can
/// re-set it in their tmux.conf after cc-hub spawns a session.
pub fn configure_clipboard() {
    if let Err(e) = run(
        &[
            "set-option",
            "-s",
            "copy-command",
            crate::clipboard::COPY_SHELL,
        ],
        "set-option copy-command",
    ) {
        warn!("configure_clipboard: {}", e);
    }
}

/// Capture the current visible content of `session`'s active pane.
/// Returns empty string on failure (mux down, session gone, etc.).
pub fn capture_pane(session: &str) -> String {
    let Ok(out) = Command::new(MUX_BIN)
        .args(["capture-pane", "-t", session, "-p"])
        .output()
    else {
        return String::new();
    };
    if !out.status.success() {
        return String::new();
    }
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// True when `session`'s pane shows claude's input prompt — meaning claude
/// is fully booted and ready to accept paste / Enter. Distinguishes a
/// "freshly spawned, JSONL not yet written" session (which the scanner
/// reports as Idle the instant it appears) from one that's actually ready
/// to receive input. Without this gate, `send_prompt` races the cold-start
/// of a worker session and the paste never lands.
///
/// claude renders `❯` at the start of an otherwise empty row to mark the
/// input box. The row sits between two `─` rule lines, with the status
/// footer (model / cost / context / hints) below it — meaning it's
/// typically 4-8 visible rows above the bottom plus several blank scrollback
/// rows after that. Cheaper to scan the whole pane than to count rows from
/// the bottom; the welcome banner uses different glyphs (`▐`, `▜`) so a
/// false positive there is unlikely.
pub fn pane_ready_for_input(session: &str) -> bool {
    pane_content_shows_empty_input(&capture_pane(session))
}

/// Pure inspector — pulled out so it's unit-testable against captured
/// fixtures. See [`pane_ready_for_input`].
pub fn pane_content_shows_empty_input(pane: &str) -> bool {
    if pane.is_empty() {
        return false;
    }
    pane.lines().any(|l| {
        let trimmed = l.trim();
        // An empty input row is exactly the `❯` glyph (after trimming the
        // single trailing space claude pads it with). A non-empty input
        // row would have user-typed text after the glyph — not what we
        // want to declare "ready" for paste.
        trimmed == "❯"
    })
}

#[cfg(test)]
mod tests {
    use super::pane_content_shows_empty_input;

    /// Real `tmux capture-pane -p` output from a freshly-spawned
    /// `cc-hub-new` session, captured during the e2e debugging session.
    /// The input row is `❯ ` — the only `❯` in the pane.
    const COLD_READY_PANE: &str = concat!(
        "╭─── Claude Code v2.1.119 ─────────────────────────────────────────────────────╮\n",
        "│                                                    │ Tips for getting        │\n",
        "│                Welcome back j-light!               │ started                 │\n",
        "│                       ▐▛███▜▌                      │                         │\n",
        "│                 /tmp/cchub-e2e-E4I                 │                         │\n",
        "╰──────────────────────────────────────────────────────────────────────────────╯\n",
        "\n",
        "────────────────────────────────────────────────────────────────────────────────\n",
        "❯ \n",
        "────────────────────────────────────────────────────────────────────────────────\n",
        "  ◆ Opus 4.7 (1M context) │ cchub-e2e-E4I │ ⎇ main │ $0.00 │ ⏱ 0s\n",
        "  ctx ╌╌╌╌╌╌╌╌╌╌ 0% │ 5h 4pm ━━╌╌╌╌╌╌╌╌ 21% │ wk mon 8am ━╌╌╌╌╌╌╌╌╌ 9%\n",
        "  ⏵⏵ bypass permissions on (shift+tab to cycle)\n",
        "\n\n\n\n\n\n",
    );

    /// Same shape, but the user has typed text into the input box.
    /// Should NOT be flagged ready — sending now would clobber the typed
    /// text or interleave with it.
    const TYPED_INTO_INPUT_PANE: &str = concat!(
        "────────────────────────────────────────────────────────────────────────────────\n",
        "❯ help me with the migration\n",
        "────────────────────────────────────────────────────────────────────────────────\n",
        "  ◆ Opus 4.7 (1M context) │ proj │ ⎇ main │ $0.00 │ ⏱ 5s\n",
    );

    /// Mid-startup, before claude has rendered the input row at all —
    /// just the welcome banner. Common for the first ~1s after spawn.
    const PRE_INPUT_BANNER_PANE: &str = concat!(
        "╭─── Claude Code v2.1.119 ─────────────────────────────────────────────────────╮\n",
        "│                Welcome back j-light!               │\n",
        "│                       ▐▛███▜▌                      │\n",
        "╰──────────────────────────────────────────────────────────────────────────────╯\n",
    );

    #[test]
    fn cold_ready_pane_is_ready() {
        assert!(pane_content_shows_empty_input(COLD_READY_PANE));
    }

    #[test]
    fn typed_input_is_not_ready() {
        assert!(!pane_content_shows_empty_input(TYPED_INTO_INPUT_PANE));
    }

    #[test]
    fn pre_input_banner_is_not_ready() {
        assert!(!pane_content_shows_empty_input(PRE_INPUT_BANNER_PANE));
    }

    #[test]
    fn empty_pane_is_not_ready() {
        assert!(!pane_content_shows_empty_input(""));
    }
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
