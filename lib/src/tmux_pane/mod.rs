//! Embed a tmux session as a live, interactive pane inside hal.
//!
//! - `osc52`: extracts clipboard escapes the vt100 parser would drop.
//! - `encode`: encodes keys and mouse buttons as xterm bytes.

use crossterm::event::{KeyEvent, KeyModifiers, MouseEvent};
use encode::{encode_key, mouse_button_code};
use log::{debug, info, warn};
use osc52::Osc52Scanner;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};

mod encode;
mod osc52;

/// psmux opens its attach handshake by querying the client's cursor
/// position and blocks until it gets an answer. Real tmux sends the same
/// query but doesn't gate the stream on the reply, so auto-answering from
/// the reader thread is a no-op on Unix and the unblock that Windows needs.
const DSR_QUERY: &[u8] = b"\x1b[6n";
const DSR_REPLY: &[u8] = b"\x1b[1;1R";

pub struct TmuxPaneView {
    pub session_name: String,
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub rows: u16,
    pub cols: u16,
    viewport_origin: (u16, u16),
    master: Box<dyn MasterPty + Send>,
    // Shared so the reader thread can auto-reply to psmux's DSR query.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// The attach client. `None` only once Drop has handed it to the
    /// thread that ends it.
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    /// The client's terminal as tmux names it (`/dev/ttys012`), which is
    /// what `switch-client -c` addresses. `None` where the pty has no name.
    tty: Option<String>,
    exited: Arc<AtomicBool>,
    /// Set by the reader thread whenever the screen took new bytes, cleared
    /// by [`Self::take_fresh`]: what lets a pane that is merely on screen
    /// repaint at the rate it draws, not at the loop's.
    fresh: Arc<AtomicBool>,
    /// OSC 52 escapes captured from the attach client's output, awaiting
    /// replay onto hal's real terminal by the main loop.
    osc52_pending: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl TmuxPaneView {
    pub fn spawn(session_name: &str, rows: u16, cols: u16) -> std::io::Result<Self> {
        // Redundant with the spawn-time enable for hal-created sessions,
        // but needed for sessions that predate that code path.
        crate::send::enable_session_mouse(session_name);
        // Server-wide options: once per hub is enough. A tmux server that
        // restarts under the hub gets them again from the next session the
        // hub spawns (`mux::spawn_detached`).
        static CLIPBOARD: Once = Once::new();
        CLIPBOARD.call_once(crate::platform::mux::configure_clipboard);

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| std::io::Error::other(format!("openpty: {}", e)))?;

        let argv = crate::platform::mux::attach_argv(session_name);
        let (bin, args) = argv
            .split_first()
            .ok_or_else(|| std::io::Error::other("empty attach argv from mux"))?;
        let mut cmd = CommandBuilder::new(bin);
        for a in args {
            cmd.arg(a);
        }
        // Inherit the user's TERM so the multiplexer picks sane capabilities.
        if let Ok(term) = std::env::var("TERM") {
            cmd.env("TERM", term);
        } else {
            cmd.env("TERM", "xterm-256color");
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| std::io::Error::other(format!("spawn tmux attach: {}", e)))?;
        // Drop the slave side so EOF propagates on master when the child exits.
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| std::io::Error::other(format!("clone reader: {}", e)))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| std::io::Error::other(format!("take writer: {}", e)))?;

        // portable-pty only names the tty on unix; a psmux client has none.
        #[cfg(unix)]
        let tty = pair
            .master
            .tty_name()
            .map(|p| p.to_string_lossy().into_owned());
        #[cfg(not(unix))]
        let tty = None;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let exited = Arc::new(AtomicBool::new(false));
        let fresh = Arc::new(AtomicBool::new(false));
        let osc52_pending: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));

        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(writer));
        let reader_writer = Arc::clone(&writer);
        {
            let parser = Arc::clone(&parser);
            let exited = Arc::clone(&exited);
            let fresh = Arc::clone(&fresh);
            let osc52_pending = Arc::clone(&osc52_pending);
            std::thread::spawn(move || {
                let mut reader = reader;
                let mut buf = [0u8; 8 * 1024];
                let mut osc52 = Osc52Scanner::new();
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => {
                            info!("tmux_pane: reader EOF");
                            break;
                        }
                        Ok(n) => {
                            if buf[..n].windows(DSR_QUERY.len()).any(|w| w == DSR_QUERY) {
                                if let Ok(mut w) = reader_writer.lock() {
                                    if let Err(e) = w.write_all(DSR_REPLY) {
                                        warn!("tmux_pane: DSR reply write failed: {}", e);
                                    } else {
                                        let _ = w.flush();
                                        debug!("tmux_pane: answered DSR query");
                                    }
                                }
                            }
                            let seqs = osc52.feed(&buf[..n]);
                            if !seqs.is_empty() {
                                debug!("tmux_pane: captured {} OSC 52 escape(s)", seqs.len());
                                if let Ok(mut q) = osc52_pending.lock() {
                                    q.extend(seqs);
                                }
                            }
                            if let Ok(mut p) = parser.lock() {
                                p.process(&buf[..n]);
                            }
                            fresh.store(true, Ordering::SeqCst);
                        }
                        Err(e) => {
                            warn!("tmux_pane: reader error: {}", e);
                            break;
                        }
                    }
                }
                exited.store(true, Ordering::SeqCst);
            });
        }

        Ok(Self {
            session_name: session_name.to_string(),
            parser,
            rows,
            cols,
            viewport_origin: (0, 0),
            master: pair.master,
            writer,
            child: Some(child),
            tty,
            exited,
            fresh,
            osc52_pending,
        })
    }

    /// Drain clipboard escapes (OSC 52) that tmux addressed to the embedded
    /// client. The vt100 parser drops them, so the main loop replays each
    /// one onto hal's own terminal — which is what lands an in-pane copy
    /// on the viewer's clipboard when hal runs on a remote box over ssh.
    pub fn take_osc52(&self) -> Vec<Vec<u8>> {
        self.osc52_pending
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default()
    }

    /// Move this client onto `session` (`tmux switch-client`): tmux
    /// repaints the new session into the same pty, so there is no client to
    /// end and none to start. Fails when the client can't be addressed (no
    /// tty name, e.g. under psmux) or when tmux refuses; the caller attaches
    /// anew.
    pub fn switch_to(&mut self, session: &str) -> std::io::Result<()> {
        if self.session_name == session {
            return Ok(());
        }
        let Some(tty) = self.tty.as_deref() else {
            return Err(std::io::Error::other("attach client has no tty name"));
        };
        crate::platform::mux::switch_client(tty, session)?;
        self.session_name = session.to_string();
        Ok(())
    }

    pub fn is_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// Whether the screen changed since the last call.
    pub fn take_fresh(&self) -> bool {
        self.fresh.swap(false, Ordering::SeqCst)
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if rows == self.rows && cols == self.cols {
            return;
        }
        if rows == 0 || cols == 0 {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        if let Err(e) = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            warn!("tmux_pane: resize master failed: {}", e);
        }
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_size(rows, cols);
        }
    }

    pub fn set_viewport_origin(&mut self, col: u16, row: u16) {
        self.viewport_origin = (col, row);
    }

    /// The pane-local cell under screen cell `(column, row)`, `None` outside
    /// the pane's current viewport.
    fn cell_at(&self, column: u16, row: u16) -> Option<(u16, u16)> {
        let (ox, oy) = self.viewport_origin;
        let x = column.checked_sub(ox)?;
        let y = row.checked_sub(oy)?;
        (x < self.cols && y < self.rows).then_some((x, y))
    }

    /// Whether screen cell `(column, row)` falls inside the pane.
    pub fn covers(&self, column: u16, row: u16) -> bool {
        self.cell_at(column, row).is_some()
    }

    /// Encode `ev` as an SGR mouse report (`CSI < b ; x ; y M/m`) and write
    /// it to the pty. Events that land outside the pane's current viewport
    /// are dropped.
    pub fn send_mouse(&mut self, ev: MouseEvent) {
        let Some((x, y)) = self.cell_at(ev.column, ev.row) else {
            return;
        };

        let Some((mut b, release)) = mouse_button_code(ev.kind) else {
            return;
        };
        if ev.modifiers.contains(KeyModifiers::SHIFT) {
            b |= 4;
        }
        if ev.modifiers.contains(KeyModifiers::ALT) {
            b |= 8;
        }
        if ev.modifiers.contains(KeyModifiers::CONTROL) {
            b |= 16;
        }
        let terminator = if release { 'm' } else { 'M' };
        let Ok(mut w) = self.writer.lock() else {
            return;
        };
        if let Err(e) = write!(w, "\x1b[<{};{};{}{}", b, x + 1, y + 1, terminator) {
            warn!("tmux_pane: mouse write failed: {}", e);
        } else {
            let _ = w.flush();
        }
    }

    pub fn send_key(&mut self, key: KeyEvent) {
        let bytes = encode_key(key);
        if bytes.is_empty() {
            return;
        }
        let Ok(mut w) = self.writer.lock() else {
            return;
        };
        if let Err(e) = w.write_all(&bytes) {
            warn!("tmux_pane: write failed: {}", e);
        } else {
            let _ = w.flush();
        }
    }

    /// Paste `text` into the pane through tmux's buffer mechanism.
    ///
    /// Writing bracketed-paste markers straight to the attach pty doesn't
    /// work: tmux's client input parser sits in between and strips or
    /// reinterprets them, so embedded newlines end up as submitted Enters.
    /// `paste-buffer -p` injects the markers at the target pane instead.
    pub fn paste_text(&self, text: &str) -> std::io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        crate::platform::mux::paste_buffer(&self.session_name, text)
    }
}

impl Drop for TmuxPaneView {
    fn drop(&mut self) {
        // `kill` sends SIGHUP and then waits up to 200ms for the client to
        // exit, in 50ms sleeps. Off the caller's thread: the dock drops a
        // pane whenever the cursor leaves its session.
        if let Some(mut child) = self.child.take() {
            std::thread::spawn(move || {
                let _ = child.kill();
                let _ = child.wait();
            });
        }
    }
}
