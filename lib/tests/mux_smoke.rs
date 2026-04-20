//! End-to-end smoke test for the mux shim on the host platform.
//! Spawns a detached session, lists it, sends keys, captures output, kills it.

use cc_hub_lib::platform::mux;
use std::process::Command;
use std::thread;
use std::time::Duration;

/// Kill the session on Drop so a mid-test panic doesn't leak psmux/tmux
/// state that trips up the next run (psmux's `list-panes -a` hides
/// sessions whose pane shell exited, which makes leftover state
/// confusing to diagnose).
struct SessionGuard(String);
impl Drop for SessionGuard {
    fn drop(&mut self) {
        let _ = mux::kill_session(&self.0);
    }
}

#[test]
fn mux_roundtrip() {
    let name = format!("cchub-smoke-{}", std::process::id());
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into());

    mux::spawn_detached(&name, &cwd, None).expect("spawn_detached");
    let _guard = SessionGuard(name.clone());
    // Give the shell a beat to settle (psmux needs pwsh cold-start; tmux is
    // instant but the extra sleep doesn't hurt).
    thread::sleep(Duration::from_millis(1500));

    let panes = mux::list_panes();
    let found = panes.iter().any(|(_, n)| n == &name);
    assert!(found, "session {} not in panes: {:?}", name, panes);

    mux::send_prompt(&name, "echo mux-smoke-ok").expect("send_prompt");
    thread::sleep(Duration::from_millis(1500));

    let out = Command::new("tmux")
        .args(["capture-pane", "-t", &name, "-p"])
        .output()
        .expect("capture-pane");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("mux-smoke-ok"),
        "expected 'mux-smoke-ok' in captured pane, got:\n{}",
        text
    );
}
