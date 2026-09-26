//! Embed a tmux session as a live, interactive pane inside cc-hub.
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
use std::sync::{Arc, Mutex};

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
    child: Box<dyn portable_pty::Child + Send + Sync>,
    exited: Arc<AtomicBool>,
    owns_session: bool,
    /// OSC 52 escapes captured from the attach client's output, awaiting
    /// replay onto cc-hub's real terminal by the main loop.
    osc52_pending: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl TmuxPaneView {
    pub fn spawn(session_name: &str, rows: u16, cols: u16) -> std::io::Result<Self> {
        // Redundant with the spawn-time enable for cc-hub-created sessions,
        // but needed for sessions that predate that code path.
        crate::send::enable_session_mouse(session_name);
        crate::platform::mux::configure_clipboard();

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

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let exited = Arc::new(AtomicBool::new(false));
        let osc52_pending: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));

        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(writer));
        let reader_writer = Arc::clone(&writer);
        {
            let parser = Arc::clone(&parser);
            let exited = Arc::clone(&exited);
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
            child,
            exited,
            owns_session: false,
            osc52_pending,
        })
    }

    /// Drain clipboard escapes (OSC 52) that tmux addressed to the embedded
    /// client. The vt100 parser drops them, so the main loop replays each
    /// one onto cc-hub's own terminal — which is what lands an in-pane copy
    /// on the viewer's clipboard when cc-hub runs on a remote box over ssh.
    pub fn take_osc52(&self) -> Vec<Vec<u8>> {
        self.osc52_pending
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default()
    }

    /// Attach like [`Self::spawn`], but take ownership of `session_name`: Drop runs
    /// `tmux kill-session`, and a construction failure kills the session before
    /// returning so the caller does not leak it.
    pub fn spawn_owned(session_name: &str, rows: u16, cols: u16) -> std::io::Result<Self> {
        match Self::spawn(session_name, rows, cols) {
            Ok(mut pane) => {
                pane.owns_session = true;
                Ok(pane)
            }
            Err(e) => {
                let _ = crate::send::kill_tmux_session(session_name);
                Err(e)
            }
        }
    }

    pub fn is_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
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

    /// Encode `ev` as an SGR mouse report (`CSI < b ; x ; y M/m`) and write
    /// it to the pty. Events that land outside the pane's current viewport
    /// are dropped.
    pub fn send_mouse(&mut self, ev: MouseEvent) {
        let (ox, oy) = self.viewport_origin;
        if ev.column < ox || ev.row < oy {
            return;
        }
        let x = ev.column - ox;
        let y = ev.row - oy;
        if x >= self.cols || y >= self.rows {
            return;
        }

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
        let _ = self.child.kill();
        if self.owns_session {
            if let Err(e) = crate::send::kill_tmux_session(&self.session_name) {
                warn!(
                    "tmux_pane: kill-session {} failed: {}",
                    self.session_name, e
                );
            }
        }
    }
}
