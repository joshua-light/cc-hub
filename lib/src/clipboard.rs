//! Host clipboard integration.
//!
//! Wraps whichever of `wl-copy`/`wl-paste`, `xclip`, or `pbcopy`/`pbpaste` is
//! installed via a small shell fallback chain. Copies additionally go out as
//! OSC 52 escapes — that is what makes them land on the *viewer's* clipboard
//! when cc-hub itself runs on a remote box over ssh (the host chain alone
//! only reaches the remote machine's clipboard) — via two routes:
//!
//! - [`copy`] writes the escape straight to cc-hub's own /dev/tty, and
//! - the composed shell command forwards through tmux `load-buffer -w` to
//!   every attached client's terminal. That command is handed to tmux's
//!   `copy-command` server option, so mouse-drag selections inside an
//!   embedded pane take the same path; the escape tmux addresses to the
//!   hub's own attach client is captured and replayed by the pane reader
//!   (see [`crate::tmux_pane::TmuxPaneView::take_osc52`]).

use std::io;
use std::io::Write;
use std::process::{Command, Stdio};

/// Shell pipeline that reads text on stdin and writes it to the host
/// clipboard.
pub const COPY_SHELL: &str =
    "wl-copy 2>/dev/null || xclip -selection clipboard -i 2>/dev/null || pbcopy 2>/dev/null";

/// Compose the copy command: stash stdin in a temp file, forward it to
/// every attached tmux client via OSC 52 (`load-buffer -w`), then run the
/// host chain on it.
///
/// The forward must target each client explicitly: bare `-w` sends the
/// escape only to tmux's notion of the current client — usually the
/// most-recently-active one, which during an embedded-pane copy is the
/// hub's own attach pty. Fanning out to all clients reaches directly
/// attached terminals; the escape sent to the hub's own attach pty is
/// captured by the pane reader and replayed onto the hub's real terminal
/// (see [`crate::tmux_pane::TmuxPaneView::take_osc52`]) — the only route
/// to the viewer's clipboard when the hub runs remotely and its embedded
/// client is the sole attachment.
///
/// `mux_bin` must be an absolute path when the string is handed to tmux's
/// `copy-command` — tmux runs it with the server's environment, whose PATH
/// may be the bare system default (e.g. a server started via ssh
/// RemoteCommand never saw the Homebrew prefix).
pub fn copy_shell_with_osc52(mux_bin: &str) -> String {
    format!(
        "tmp=$(mktemp) || exit 1; cat >\"$tmp\"; \
         \"{mux_bin}\" load-buffer -b cchub-clip \"$tmp\" 2>/dev/null; \
         for c in $(\"{mux_bin}\" list-clients -F \"#{{client_name}}\" 2>/dev/null); do \
         \"{mux_bin}\" load-buffer -b cchub-clip -w -t \"$c\" \"$tmp\" 2>/dev/null; done; \
         ({COPY_SHELL}) <\"$tmp\"; rm -f \"$tmp\""
    )
}

const PASTE_SHELL: &str =
    "wl-paste 2>/dev/null || xclip -selection clipboard -o 2>/dev/null || pbpaste 2>/dev/null";

/// Read the host clipboard. An empty result is indistinguishable from "no
/// backend installed" — both callers treat it as a no-op paste.
pub fn paste() -> io::Result<String> {
    let out = Command::new("sh").arg("-c").arg(PASTE_SHELL).output()?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Write `text` to the host clipboard via the same fallback chain used by
/// tmux, plus the OSC 52 forward for remote viewers. Silently no-ops if no
/// backend is installed (the chain in [`COPY_SHELL`] short-circuits to
/// success after each `2>/dev/null`). Plain "tmux" is fine here: this runs
/// with cc-hub's own PATH, not the tmux server's.
pub fn copy(text: &str) -> io::Result<()> {
    // The tmux fan-out below only reaches *attached* clients — with no
    // session open (e.g. copying a task id from the grid) there are none,
    // and the host chain alone can't cross an ssh boundary. Emitting the
    // escape on our own tty covers that case unconditionally.
    if !text.is_empty() {
        osc52_to_tty(text);
    }
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(copy_shell_with_osc52("tmux"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(text.as_bytes())?;
    }
    let _ = child.wait()?;
    Ok(())
}

/// Emit `text` to the controlling terminal as an OSC 52 escape, so the
/// *viewer's* clipboard is set even when cc-hub runs on a remote box over
/// ssh — the host chain in [`COPY_SHELL`] only reaches the machine cc-hub
/// runs on. Goes to /dev/tty rather than stdout so it bypasses whatever
/// the TUI backend is doing with stdout. Best-effort: no controlling
/// terminal means the host chain is the only path. If cc-hub itself runs
/// inside a tmux, that tmux forwards the escape outward (`set-clipboard`
/// defaults to `external`).
#[cfg(unix)]
fn osc52_to_tty(text: &str) {
    let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") else {
        return;
    };
    let _ = write!(tty, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
}

/// No /dev/tty on Windows; the escape would also have to survive ConPTY.
/// Copies there rely on the host chain and the tmux fan-out.
#[cfg(not(unix))]
fn osc52_to_tty(_text: &str) {}

/// Standard-alphabet base64 with padding — hand-rolled so one escape
/// sequence doesn't pull in a crate.
#[cfg(unix)]
fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = u32::from_be_bytes([
            0,
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ]);
        out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(all(test, unix))]
mod tests {
    use super::base64;

    #[test]
    fn base64_matches_known_vectors() {
        // RFC 4648 test vectors.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
