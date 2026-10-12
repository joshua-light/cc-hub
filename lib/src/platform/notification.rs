//! Desktop notifications: how a process nobody watches — a URL-scheme
//! handler, a detached job — still reaches the user. macOS posts through
//! `osascript`, Linux through `notify-send`; elsewhere a notification is
//! reported as unsupported.

use std::io;
use std::process::Command;

/// Post `text` under `title` to the desktop's notification centre.
pub fn post(title: &str, text: &str) -> io::Result<()> {
    let status = command(title, text)?.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "notification exited with {status}"
        )))
    }
}

/// The text travels as `argv`, not spliced into the script, so no quote in
/// an error message can break out of the AppleScript string.
#[cfg(target_os = "macos")]
fn command(title: &str, text: &str) -> io::Result<Command> {
    let mut cmd = Command::new("osascript");
    cmd.args([
        "-e",
        "on run argv",
        "-e",
        "display notification (item 2 of argv) with title (item 1 of argv)",
        "-e",
        "end run",
        title,
        text,
    ]);
    Ok(cmd)
}

#[cfg(target_os = "linux")]
fn command(title: &str, text: &str) -> io::Result<Command> {
    let mut cmd = Command::new("notify-send");
    cmd.args(["--", title, text]);
    Ok(cmd)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn command(_title: &str, _text: &str) -> io::Result<Command> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no desktop notifications on this platform",
    ))
}
