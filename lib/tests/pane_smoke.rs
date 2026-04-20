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

#[test]
fn pane_attach_delivers_bytes() {
    let name = format!("cchub-pane-raw-{}", std::process::id());
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into());

    mux::spawn_detached(&name, &cwd, None).expect("spawn_detached");
    let _guard = SessionGuard(name.clone());
    thread::sleep(Duration::from_millis(1500));

    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
        .expect("openpty");

    let argv = mux::attach_argv(&name);
    let (bin, args) = argv.split_first().unwrap();
    let mut cmd = CommandBuilder::new(bin);
    for a in args {
        cmd.arg(a);
    }
    cmd.env("TERM", "xterm-256color");

    let child = Arc::new(Mutex::new(pair.slave.spawn_command(cmd).expect("spawn attach")));
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
