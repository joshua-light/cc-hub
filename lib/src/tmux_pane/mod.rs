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

const OSC52_PREFIX: &[u8] = b"\x1b]52;";
/// Abandon a sequence that grows past this — no sane clipboard payload is
/// this large, and it bounds memory if a terminator never arrives.
const OSC52_MAX: usize = 1024 * 1024;

/// Incremental extractor for OSC 52 (clipboard) escapes in the attach
/// client's output stream.
///
/// tmux delivers copies to its clients as OSC 52, but the embedded client's
/// output goes to the vt100 parser, which doesn't understand the sequence
/// and silently drops it. The reader thread runs every chunk through this
/// scanner so the escapes can be replayed onto cc-hub's real terminal —
/// the hop that lands an in-pane copy on the *viewer's* clipboard when
/// cc-hub itself runs on a remote box over ssh (tmux's copy-command can
/// only reach the remote host's clipboard).
///
/// Stateful across `feed` calls: a large selection easily out-sizes one
/// 8 KiB pty read, so a per-chunk scan would miss split sequences.
struct Osc52Scanner {
    state: Osc52State,
}

enum Osc52State {
    /// Matching `\x1b]52;`; holds how many prefix bytes matched so far.
    Prefix(usize),
    /// Inside the sequence, accumulating the full escape (prefix included).
    Body(Vec<u8>),
    /// Saw ESC inside the body; the next byte decides ST (`\`) or abort.
    BodyEsc(Vec<u8>),
}

impl Osc52Scanner {
    fn new() -> Self {
        Self {
            state: Osc52State::Prefix(0),
        }
    }

    /// Consume a chunk, returning every complete OSC 52 escape it finished.
    fn feed(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for &b in chunk {
            self.state = match std::mem::replace(&mut self.state, Osc52State::Prefix(0)) {
                Osc52State::Prefix(n) => match_prefix(n, b),
                Osc52State::Body(mut buf) => {
                    if b == 0x07 {
                        buf.push(0x07);
                        push_unless_query(&mut out, buf);
                        Osc52State::Prefix(0)
                    } else if b == 0x1b {
                        Osc52State::BodyEsc(buf)
                    } else if buf.len() >= OSC52_MAX {
                        Osc52State::Prefix(0)
                    } else {
                        buf.push(b);
                        Osc52State::Body(buf)
                    }
                }
                Osc52State::BodyEsc(mut buf) => {
                    if b == b'\\' {
                        buf.extend_from_slice(b"\x1b\\");
                        push_unless_query(&mut out, buf);
                        Osc52State::Prefix(0)
                    } else {
                        // The ESC aborted this sequence but may itself open
                        // a new escape — resume matching after it.
                        match_prefix(1, b)
                    }
                }
            };
        }
        out
    }
}

/// One step of prefix matching: `n` bytes already matched, `b` is next.
fn match_prefix(n: usize, b: u8) -> Osc52State {
    if b == OSC52_PREFIX[n] {
        if n + 1 == OSC52_PREFIX.len() {
            Osc52State::Body(OSC52_PREFIX.to_vec())
        } else {
            Osc52State::Prefix(n + 1)
        }
    } else if b == OSC52_PREFIX[0] {
        Osc52State::Prefix(1)
    } else {
        Osc52State::Prefix(0)
    }
}

/// Queue a finished escape unless it's a clipboard *query* (payload `?`):
/// replaying a query would make the host terminal answer on cc-hub's
/// stdin, where crossterm would misread the reply as key input.
fn push_unless_query(out: &mut Vec<Vec<u8>>, seq: Vec<u8>) {
    let body = match seq.last() {
        Some(&0x07) => &seq[..seq.len() - 1],
        _ => &seq[..seq.len().saturating_sub(2)],
    };
    if !body.ends_with(b";?") {
        out.push(seq);
    }
}

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
        MouseEventKind::Moved | MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => None,
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

#[cfg(test)]
mod tests {
    use super::Osc52Scanner;

    #[test]
    fn extracts_bel_terminated_sequence() {
        let mut s = Osc52Scanner::new();
        let out = s.feed(b"before\x1b]52;c;aGVsbG8=\x07after");
        assert_eq!(out, vec![b"\x1b]52;c;aGVsbG8=\x07".to_vec()]);
    }

    #[test]
    fn extracts_st_terminated_sequence() {
        let mut s = Osc52Scanner::new();
        let out = s.feed(b"\x1b]52;c;aGVsbG8=\x1b\\tail");
        assert_eq!(out, vec![b"\x1b]52;c;aGVsbG8=\x1b\\".to_vec()]);
    }

    /// tmux's Ms emission for a big selection easily spans several pty
    /// reads — the scanner must carry its state across chunks.
    #[test]
    fn reassembles_sequence_split_across_chunks() {
        let mut s = Osc52Scanner::new();
        assert!(s.feed(b"x\x1b]5").is_empty());
        assert!(s.feed(b"2;c;aGVs").is_empty());
        let out = s.feed(b"bG8=\x07y");
        assert_eq!(out, vec![b"\x1b]52;c;aGVsbG8=\x07".to_vec()]);
    }

    #[test]
    fn ignores_other_osc_sequences() {
        let mut s = Osc52Scanner::new();
        assert!(s.feed(b"\x1b]0;window title\x07").is_empty());
        // ...and stays in sync for a following OSC 52.
        let out = s.feed(b"\x1b]52;c;QQ==\x07");
        assert_eq!(out.len(), 1);
    }

    /// A clipboard *query* must not be replayed: the host terminal would
    /// answer it on cc-hub's stdin.
    #[test]
    fn drops_clipboard_queries() {
        let mut s = Osc52Scanner::new();
        assert!(s.feed(b"\x1b]52;c;?\x07").is_empty());
        assert!(s.feed(b"\x1b]52;;?\x1b\\").is_empty());
    }

    #[test]
    fn esc_aborting_body_can_open_next_sequence() {
        let mut s = Osc52Scanner::new();
        // First sequence is malformed (ESC not followed by `\`), second
        // starts at that ESC and must still be captured.
        let out = s.feed(b"\x1b]52;c;abc\x1b]52;c;QQ==\x07");
        assert_eq!(out, vec![b"\x1b]52;c;QQ==\x07".to_vec()]);
    }
}
