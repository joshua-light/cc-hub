//! Embed a tmux session as a live, interactive pane inside cc-hub.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use log::{debug, info, warn};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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

        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(writer));
        let reader_writer = Arc::clone(&writer);
        {
            let parser = Arc::clone(&parser);
            let exited = Arc::clone(&exited);
            std::thread::spawn(move || {
                let mut reader = reader;
                let mut buf = [0u8; 8 * 1024];
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
        })
    }

    /// Attach like [`spawn`], but take ownership of `session_name`: Drop runs
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
        let Ok(mut w) = self.writer.lock() else { return };
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
        let Ok(mut w) = self.writer.lock() else { return };
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
                warn!("tmux_pane: kill-session {} failed: {}", self.session_name, e);
            }
        }
    }
}

fn encode_key(key: KeyEvent) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    match key.code {
        KeyCode::Char(c) => {
            let mut out = Vec::new();
            if alt {
                out.push(0x1b);
            }
            if ctrl {
                let b = match c {
                    ' ' => 0x00,
                    '@' => 0x00,
                    c if c.is_ascii_alphabetic() => (c.to_ascii_uppercase() as u8) & 0x1f,
                    '[' => 0x1b,
                    '\\' => 0x1c,
                    ']' => 0x1d,
                    '^' => 0x1e,
                    '_' | '?' => 0x1f,
                    _ => return Vec::new(),
                };
                out.push(b);
            } else {
                let mut tmp = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
            }
            out
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => {
            if shift {
                b"\x1b[Z".to_vec()
            } else {
                vec![b'\t']
            }
        }
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Left => csi_arrow(b'D', &key.modifiers),
        KeyCode::Right => csi_arrow(b'C', &key.modifiers),
        KeyCode::Up => csi_arrow(b'A', &key.modifiers),
        KeyCode::Down => csi_arrow(b'B', &key.modifiers),
        KeyCode::Home => csi_arrow(b'H', &key.modifiers),
        KeyCode::End => csi_arrow(b'F', &key.modifiers),
        KeyCode::PageUp => csi_tilde(5, &key.modifiers),
        KeyCode::PageDown => csi_tilde(6, &key.modifiers),
        KeyCode::Insert => csi_tilde(2, &key.modifiers),
        KeyCode::Delete => csi_tilde(3, &key.modifiers),
        KeyCode::F(n) => function_key(n),
        KeyCode::Null => Vec::new(),
        _ => Vec::new(),
    }
}

fn modifier_code(mods: &KeyModifiers) -> u8 {
    // xterm modifier encoding: 1 + shift(1) + alt(2) + ctrl(4)
    let mut m = 0u8;
    if mods.contains(KeyModifiers::SHIFT) {
        m |= 1;
    }
    if mods.contains(KeyModifiers::ALT) {
        m |= 2;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        m |= 4;
    }
    m + 1
}

fn csi_arrow(letter: u8, mods: &KeyModifiers) -> Vec<u8> {
    let m = modifier_code(mods);
    if m == 1 {
        vec![0x1b, b'[', letter]
    } else {
        format!("\x1b[1;{}{}", m, letter as char).into_bytes()
    }
}

fn csi_tilde(code: u8, mods: &KeyModifiers) -> Vec<u8> {
    let m = modifier_code(mods);
    if m == 1 {
        format!("\x1b[{}~", code).into_bytes()
    } else {
        format!("\x1b[{};{}~", code, m).into_bytes()
    }
}

fn mouse_button_code(kind: MouseEventKind) -> Option<(u32, bool)> {
    match kind {
        MouseEventKind::Down(b) => Some((button_base(b), false)),
        MouseEventKind::Up(b) => Some((button_base(b), true)),
        MouseEventKind::Drag(b) => Some((button_base(b) | 32, false)),
        MouseEventKind::ScrollUp => Some((64, false)),
        MouseEventKind::ScrollDown => Some((65, false)),
        // Plain motion and horizontal scroll would flood the pty and tmux
        // does nothing useful with them.
        MouseEventKind::Moved
        | MouseEventKind::ScrollLeft
        | MouseEventKind::ScrollRight => None,
    }
}

fn button_base(b: MouseButton) -> u32 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

fn function_key(n: u8) -> Vec<u8> {
    match n {
        1 => b"\x1bOP".to_vec(),
        2 => b"\x1bOQ".to_vec(),
        3 => b"\x1bOR".to_vec(),
        4 => b"\x1bOS".to_vec(),
        5 => b"\x1b[15~".to_vec(),
        6 => b"\x1b[17~".to_vec(),
        7 => b"\x1b[18~".to_vec(),
        8 => b"\x1b[19~".to_vec(),
        9 => b"\x1b[20~".to_vec(),
        10 => b"\x1b[21~".to_vec(),
        11 => b"\x1b[23~".to_vec(),
        12 => b"\x1b[24~".to_vec(),
        _ => Vec::new(),
    }
}
