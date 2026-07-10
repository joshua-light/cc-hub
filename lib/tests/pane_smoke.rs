//! Verify the host-platform PTY (ConPTY on Windows) delivers bytes from a
//! `tmux attach` child. The reader is blocking, so we arm a timer thread
//! that kills the attach child after a deadline — that EOFs the master and
//! the reader thread exits cleanly.

use cc_hub_lib::platform::mux;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

struct SessionGuard(String);
impl Drop for SessionGuard {
    fn drop(&mut self) {
        let _ = mux::kill_session(&self.0);
    }
}

/// The mux shim is driven through the `tmux` binary (psmux's `tmux.exe` shim
/// on Windows). Skip the smoke test silently when it's not on `PATH` so the
/// suite stays passable on bare CI images.
fn tmux_available() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn pane_attach_delivers_bytes() {
    if !tmux_available() {
        eprintln!("pane_attach_delivers_bytes: tmux not on PATH, skipping");
        return;
    }
    let name = format!("cchub-pane-raw-{}", std::process::id());
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into());

    mux::spawn_detached(&name, &cwd, None).expect("spawn_detached");
    let _guard = SessionGuard(name.clone());
    thread::sleep(Duration::from_millis(1500));

    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let argv = mux::attach_argv(&name);
    let (bin, args) = argv.split_first().unwrap();
    let mut cmd = CommandBuilder::new(bin);
    for a in args {
        cmd.arg(a);
    }
    cmd.env("TERM", "xterm-256color");

    let child = Arc::new(Mutex::new(
        pair.slave.spawn_command(cmd).expect("spawn attach"),
    ));
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("clone reader");

    thread::sleep(Duration::from_millis(500));
    let _ = mux::send_prompt(&name, "echo pane-raw-ok");

    // Windows ConPTY doesn't always propagate EOF when the child is killed,
    // so we also drop the master from the deadline worker — that surfaces
    // as a read error, which breaks the loop.
    let master_cell: Arc<Mutex<Option<_>>> = Arc::new(Mutex::new(Some(pair.master)));
    let child_killer = Arc::clone(&child);
    let master_killer = Arc::clone(&master_cell);
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(4));
        let _ = child_killer.lock().unwrap().kill();
        thread::sleep(Duration::from_millis(200));
        let _ = master_killer.lock().unwrap().take();
    });

    let start = Instant::now();
    let mut got = Vec::<u8>::new();
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                println!("pane_smoke: EOF after {:?}", start.elapsed());
                break;
            }
            Ok(n) => {
                got.extend_from_slice(&buf[..n]);
                if got.len() > 16_384 {
                    let _ = child.lock().unwrap().kill();
                }
            }
            Err(e) => {
                println!("pane_smoke: reader err after {:?}: {}", start.elapsed(), e);
                break;
            }
        }
    }

    assert!(!got.is_empty(), "PTY delivered zero bytes from tmux attach");
}

/// End-to-end check of the clipboard relay for remote viewers: tmux's
/// `load-buffer -w -t <client>` (the same fan-out `copy-command` composes)
/// must surface via [`TmuxPaneView::take_osc52`] so the main loop can
/// replay it onto the hub's real terminal. This is the only route to the
/// viewer's clipboard when the embedded client is the session's sole
/// attachment — the exact shape of "ssh into a box, run cc-hub there".
#[test]
fn embedded_pane_captures_osc52_copy() {
    use cc_hub_lib::tmux_pane::TmuxPaneView;

    if !tmux_available() {
        eprintln!("embedded_pane_captures_osc52_copy: tmux not on PATH, skipping");
        return;
    }
    let name = format!("cchub-pane-osc52-{}", std::process::id());
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into());

    mux::spawn_detached(&name, &cwd, None).expect("spawn_detached");
    let _guard = SessionGuard(name.clone());
    thread::sleep(Duration::from_millis(1500));

    let pane = TmuxPaneView::spawn(&name, 24, 80).expect("spawn pane");

    // Wait until tmux reports the embedded attach as a client — the -w
    // forward below is addressed per-client, so firing before the attach
    // registers would reach nobody.
    let client = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let out = std::process::Command::new("tmux")
                .args(["list-clients", "-t", &name, "-F", "#{client_name}"])
                .output()
                .expect("list-clients");
            let first = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .map(str::to_string);
            if let Some(c) = first {
                break c;
            }
            assert!(Instant::now() < deadline, "attach client never registered");
            thread::sleep(Duration::from_millis(100));
        }
    };

    // The fan-out step of `clipboard::copy_shell_with_osc52`, minus the
    // host-chain tail so the test doesn't clobber the dev machine's real
    // clipboard.
    let payload = "osc52-relay-check";
    let tmp = std::env::temp_dir().join(format!("{}.txt", name));
    std::fs::write(&tmp, payload).expect("write payload");
    let status = std::process::Command::new("tmux")
        .args(["load-buffer", "-b", "cchub-clip", "-w", "-t", &client])
        .arg(&tmp)
        .status()
        .expect("load-buffer -w");
    let _ = std::fs::remove_file(&tmp);
    assert!(status.success(), "load-buffer -w failed");

    let deadline = Instant::now() + Duration::from_secs(5);
    let seqs = loop {
        let seqs = pane.take_osc52();
        if !seqs.is_empty() {
            break seqs;
        }
        assert!(
            Instant::now() < deadline,
            "no OSC 52 escape captured from the embedded client \
             (is `tmux set-clipboard` off, or tmux < 3.2?)"
        );
        thread::sleep(Duration::from_millis(100));
    };

    // base64("osc52-relay-check") — the escape must carry the payload
    // verbatim so the viewer's terminal decodes the same text.
    let expected = "b3NjNTItcmVsYXktY2hlY2s=";
    let joined = String::from_utf8_lossy(&seqs.concat()).into_owned();
    assert!(
        joined.contains(expected),
        "captured escape(s) missing payload: {:?}",
        joined
    );
}
