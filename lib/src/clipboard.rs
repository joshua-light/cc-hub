//! Host clipboard integration.
//!
//! Wraps whichever of `wl-copy`/`wl-paste`, `xclip`, or `pbcopy`/`pbpaste` is
//! installed via a small shell fallback chain. Copies additionally go through
//! tmux `load-buffer -w`, which forwards the text to every attached client's
//! terminal as an OSC 52 escape — that is what makes copies land on the
//! *viewer's* clipboard when cc-hub itself runs on a remote box over ssh
//! (the host chain alone would only reach the remote machine's clipboard).
//! The composed command is also handed to tmux's `copy-command` server
//! option so mouse-drag selections inside an embedded pane behave the same.

use std::io;
use std::io::Write;
use std::process::{Command, Stdio};

/// Shell pipeline that reads text on stdin and writes it to the host
/// clipboard.
pub const COPY_SHELL: &str =
    "wl-copy 2>/dev/null || xclip -selection clipboard -i 2>/dev/null || pbcopy 2>/dev/null";

/// Compose the copy command: stash stdin in a temp file, forward it to all
/// attached tmux clients via OSC 52 (`load-buffer -w`), then run the host
/// chain on it. `mux_bin` must be an absolute path when the string is handed
/// to tmux's `copy-command` — tmux runs it with the server's environment,
/// whose PATH may be the bare system default (e.g. a server started via ssh
/// RemoteCommand never saw the Homebrew prefix).
pub fn copy_shell_with_osc52(mux_bin: &str) -> String {
    format!(
        "tmp=$(mktemp) || exit 1; cat >\"$tmp\"; \
         \"{mux_bin}\" load-buffer -w \"$tmp\" 2>/dev/null; \
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
