use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Byte-counting wrapper around the terminal writer. Every escape byte
/// ratatui flushes passes through here, so the draw trace can report each
/// frame's real diff size — the work the host terminal (or tmux/ssh hop)
/// must do to paint it. The counter is shared out through an `Arc` because
/// ratatui owns the writer once the backend is built.
pub(crate) struct CountingWriter<W> {
    inner: W,
    written: Arc<AtomicU64>,
}

impl<W> CountingWriter<W> {
    fn new(inner: W, written: Arc<AtomicU64>) -> Self {
        Self { inner, written }
    }
}

impl<W: io::Write> io::Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The TUI's terminal, with the byte-counting writer under the backend.
pub(crate) type Term = Terminal<CrosstermBackend<CountingWriter<io::Stdout>>>;

/// Which optional terminal modes [`enter`] managed to switch on, so [`leave`]
/// turns off exactly those.
pub(crate) struct Modes {
    bracketed_paste: bool,
    kb_enhanced: bool,
    focus_events: bool,
}

/// Switch the terminal into TUI mode and build the ratatui terminal. Returns
/// the frame-diff byte counter that the draw trace drains once per draw.
pub(crate) fn enter() -> io::Result<(Term, Arc<AtomicU64>, Modes)> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    install_panic_hook();
    // Best-effort: kitty-protocol disambiguation makes Ctrl+Shift+V report
    // the SHIFT modifier (plain xterm folds it into Ctrl+V). Silently
    // ignored by terminals that don't implement the protocol.
    let kb_enhanced = crossterm::execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
    .is_ok();
    // Most terminals intercept Ctrl+Shift+V themselves and "type" the
    // clipboard as individual keystrokes — which breaks multi-line pastes
    // because embedded newlines arrive as Enter. Enabling bracketed-paste
    // mode tells the host terminal to wrap pastes in markers so crossterm
    // surfaces them as a single `Event::Paste(String)` instead.
    let bracketed_paste = crossterm::execute!(stdout, EnableBracketedPaste).is_ok();
    // Focus reporting: coming back to the hub (Mission Control swipe, cmd-tab,
    // tmux window switch) delivers `Event::FocusGained`, which the loop turns
    // into a full clear + repaint. Ratatui only ever writes the cells that
    // changed since its last frame, so whatever the host terminal dropped
    // while the window was hidden or animating stays black until something
    // forces every cell out again. Terminals without focus reporting ignore
    // the request; tmux forwards it only with `focus-events on`.
    let focus_events = crossterm::execute!(stdout, EnableFocusChange).is_ok();
    // Drained once per draw for the `bytes=` field of the draw trace.
    let frame_bytes = Arc::new(AtomicU64::new(0));
    let backend = CrosstermBackend::new(CountingWriter::new(stdout, Arc::clone(&frame_bytes)));
    let terminal = Terminal::new(backend)?;
    let modes = Modes {
        bracketed_paste,
        kb_enhanced,
        focus_events,
    };
    Ok((terminal, frame_bytes, modes))
}

/// Undo [`enter`] and show the cursor again.
pub(crate) fn leave(terminal: &mut Term, modes: &Modes) -> io::Result<()> {
    restore_terminal(
        terminal.backend_mut(),
        modes.bracketed_paste,
        modes.kb_enhanced,
        modes.focus_events,
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn restore_terminal<W: io::Write>(
    out: &mut W,
    bracketed_paste: bool,
    kb_enhanced: bool,
    focus_events: bool,
) -> io::Result<()> {
    let _ = crossterm::execute!(out, DisableMouseCapture);
    if bracketed_paste {
        let _ = crossterm::execute!(out, DisableBracketedPaste);
    }
    if focus_events {
        let _ = crossterm::execute!(out, DisableFocusChange);
    }
    if kb_enhanced {
        let _ = crossterm::execute!(out, PopKeyboardEnhancementFlags);
    }
    terminal::disable_raw_mode()?;
    crossterm::execute!(out, LeaveAlternateScreen)?;
    Ok(())
}

/// Best-effort terminal restore if anything panics (including inside tokio
/// tasks). Without this, a panic mid-run leaves the terminal in raw mode +
/// alt screen with no cursor — user has to blindly type `reset` to recover.
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = io::stdout();
        let _ = restore_terminal(&mut out, true, true, true);
        prev(info);
    }));
}
