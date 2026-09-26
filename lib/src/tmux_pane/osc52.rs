//! OSC 52 (clipboard) extraction from the attach client's output stream.

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
pub(super) struct Osc52Scanner {
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
    pub(super) fn new() -> Self {
        Self {
            state: Osc52State::Prefix(0),
        }
    }

    /// Consume a chunk, returning every complete OSC 52 escape it finished.
    pub(super) fn feed(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
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
