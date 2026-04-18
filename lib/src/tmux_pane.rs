//! Embed a tmux session as a live, interactive pane inside cc-hub.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use log::{info, warn};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct TmuxPaneView {
    pub session_name: String,
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub rows: u16,
    pub cols: u16,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    exited: Arc<AtomicBool>,
}

impl TmuxPaneView {
    pub fn spawn(session_name: &str, rows: u16, cols: u16) -> std::io::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| std::io::Error::other(format!("openpty: {}", e)))?;

        let mut cmd = CommandBuilder::new("tmux");
        cmd.arg("attach");
        cmd.arg("-t");
        cmd.arg(session_name);
        // Inherit the user's TERM so tmux picks sane capabilities.
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

        // Portable-pty readers are sync, so dedicate a blocking thread.
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
            master: pair.master,
            writer,
            child,
            exited,
        })
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

    pub fn send_key(&mut self, key: KeyEvent) {
        let bytes = encode_key(key);
        if bytes.is_empty() {
            return;
        }
        if let Err(e) = self.writer.write_all(&bytes) {
            warn!("tmux_pane: write failed: {}", e);
        } else {
            let _ = self.writer.flush();
        }
    }

}

impl Drop for TmuxPaneView {
    fn drop(&mut self) {
        let _ = self.child.kill();
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
