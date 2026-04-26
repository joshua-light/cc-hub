//! Host clipboard integration.
//!
//! Wraps whichever of `wl-copy`/`wl-paste`, `xclip`, or `pbcopy`/`pbpaste` is
//! installed via a small shell fallback chain. The same `COPY_SHELL` string
//! is handed to tmux's `copy-command` server option so mouse-drag selections
//! inside an embedded pane land in the host clipboard too.

use std::io;
use std::io::Write;
use std::process::{Command, Stdio};

/// Shell pipeline that reads text on stdin and writes it to the host
/// clipboard. Exposed for tmux `copy-command` wiring.
pub const COPY_SHELL: &str =
    "wl-copy 2>/dev/null || xclip -selection clipboard -i 2>/dev/null || pbcopy 2>/dev/null";

const PASTE_SHELL: &str =
    "wl-paste 2>/dev/null || xclip -selection clipboard -o 2>/dev/null || pbpaste 2>/dev/null";

/// Read the host clipboard. An empty result is indistinguishable from "no
/// backend installed" — both callers treat it as a no-op paste.
pub fn paste() -> io::Result<String> {
    let out = Command::new("sh").arg("-c").arg(PASTE_SHELL).output()?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Write `text` to the host clipboard via the same fallback chain used by
/// tmux. Silently no-ops if no backend is installed (the chain in
/// [`COPY_SHELL`] short-circuits to success after each `2>/dev/null`).
pub fn copy(text: &str) -> io::Result<()> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(COPY_SHELL)
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
