//! crossterm events to the xterm byte sequences the attach client expects.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};

pub(super) fn encode_key(key: KeyEvent) -> Vec<u8> {
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

pub(super) fn mouse_button_code(kind: MouseEventKind) -> Option<(u32, bool)> {
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
