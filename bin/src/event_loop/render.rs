use crate::term::Term;
use cc_hub_lib::app::{App, View};
use std::io;
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Minimum spacing between terminal round-trip probes, so a key mash
/// costs at most one probe every 2s.
const PROBE_INTERVAL: Duration = Duration::from_secs(2);

/// Redraw gating and draw telemetry carried across loop passes.
pub(super) struct Redraw {
    // Redraw gating. The loop wakes every ~5ms but the widget
    // tree only changes on input, on a drained ScanMsg, on a LiveTail poll
    // that picked up new entries, or on the once-per-second elapsed clock.
    // We draw immediately on the first three (so input latency is unchanged)
    // and throttle the purely time-driven "refreshed Ns ago" / elapsed-card
    // repaint to ~1Hz. `dirty` starts true so the first frame always renders.
    // The embedded tmux pane is the exception: its content is fed by a
    // background reader, so while it's open we redraw every loop tick.
    pub(super) dirty: bool,
    last_clock_redraw: Instant,
    // Set when an input event is handled, consumed by the draw that follows:
    // the resulting `latency=` in the draw trace is the app-side time from
    // event read to frame flushed. If a user-felt lag isn't visible here,
    // the time is being lost outside the process (terminal, compositor).
    pub(super) last_input_at: Option<Instant>,
    // Full-repaint requests, against black stripes after a Mission Control
    // swipe on a single fullscreen monitor. Ratatui's `autoresize` compares
    // the terminal size against the size of the *last drawn* frame, so a
    // resize that bounces back before the next draw (the swipe produces
    // Resize pairs ~5ms apart: 244x76 -> 131x35 -> ... -> 244x76) is
    // invisible to it — no clear, diff-only frame onto a screen the
    // terminal/tmux already wiped, and only the cells that changed since
    // the previous frame show. `full_redraw` makes the next frame start
    // from `Terminal::clear()` regardless of what `autoresize` thinks.
    pub(super) full_redraw: bool,
    // A resize storm's first frame can land while the window is still
    // animating; the terminal may repaint over it afterwards. Once no Resize
    // has arrived for `input::RESIZE_SETTLE` we repaint in full a second time.
    pub(super) resize_settle_at: Option<Instant>,
    // Terminal round-trip probe state. In-process telemetry keeps proving
    // the loop innocent while users still feel 500-1000ms on a keypress, so
    // after an input-triggered frame is flushed we ask the terminal where
    // its cursor is (CPR, ESC[6n) and time the answer. The reply can only
    // arrive after the terminal has consumed everything we wrote, so the
    // round trip exposes a backlogged terminal/tmux/ssh hop — the part of
    // the pipeline no in-process timer can see. One failed probe disables
    // probing for the rest of the run: a terminal that doesn't answer CPR
    // would otherwise cost a 2s blocking timeout per probe.
    probe_ok: bool,
    last_probe: Instant,
    // The pane redraws at stream rate and its per-frame trace is skipped to
    // keep the log readable; aggregate its flushed bytes and report ~1Hz so
    // pane-driven terminal load still shows up in forensics.
    pane_bytes: u64,
    last_pane_bytes_log: Instant,
}

impl Redraw {
    pub(super) fn new() -> Self {
        Self {
            dirty: true,
            last_clock_redraw: Instant::now(),
            last_input_at: None,
            full_redraw: false,
            resize_settle_at: None,
            probe_ok: true,
            last_probe: Instant::now(),
            pane_bytes: 0,
            last_pane_bytes_log: Instant::now(),
        }
    }
}

/// Draw a frame when anything asks for one, trace it, and probe the
/// terminal after an input-triggered frame. Returns the time spent drawing.
pub(super) fn draw(
    terminal: &mut Term,
    app: &mut App,
    frame_bytes: &AtomicU64,
    r: &mut Redraw,
) -> io::Result<Duration> {
    // The embedded tmux pane streams content from a background reader, so
    // its widget tree changes without any event we observe here — always
    // repaint while it's open. Otherwise paint only when something
    // changed, plus a ~1Hz tick so the elapsed clocks keep moving.
    let in_tmux = app.view == View::TmuxPane;
    let clock_tick = r.last_clock_redraw.elapsed() >= Duration::from_secs(1);
    if r.resize_settle_at.is_some_and(|t| Instant::now() >= t) {
        r.resize_settle_at = None;
        r.full_redraw = true;
        log::debug!("event: resize settled — full redraw");
    }
    let t_draw = Instant::now();
    let mut draw_dur = Duration::ZERO;
    if r.dirty || in_tmux || clock_tick || r.full_redraw {
        if r.full_redraw {
            // Wipes the terminal and resets ratatui's back buffer, so
            // the draw below emits every non-blank cell instead of a
            // diff against a frame the screen no longer shows.
            terminal.clear()?;
        }
        terminal.draw(|frame| cc_hub_lib::ui::render(frame, app))?;
        draw_dur = t_draw.elapsed();
        // Diff size this frame pushed at the terminal. Drained even for
        // pane frames so their bytes can't leak into the next Grid
        // frame's number.
        let flushed = frame_bytes.swap(0, Ordering::Relaxed);
        let input_latency = if in_tmux {
            None
        } else {
            r.last_input_at.take().map(|t| t.elapsed())
        };
        // Trace what each frame actually rendered (key/selection traces
        // alone proved state correct while a user still saw a stale
        // highlight — this line closes the state↔pixels gap). `latency`
        // is read-to-frame for the most recent input event: app-side
        // responsiveness, measured per keypress. Skip the pane's 60Hz
        // stream to keep the log readable.
        if !in_tmux {
            log::debug!(
                "draw: sel=({}, {}) view={:?} trigger={} took={:?} bytes={} latency={:?}",
                app.sessions.sel_group,
                app.sessions.sel_in_group,
                app.view,
                if r.full_redraw {
                    "full"
                } else if r.dirty {
                    "dirty"
                } else {
                    "clock"
                },
                draw_dur,
                flushed,
                input_latency,
            );
        } else {
            r.pane_bytes = r.pane_bytes.saturating_add(flushed);
            if r.last_pane_bytes_log.elapsed() >= Duration::from_secs(1) {
                log::debug!(
                    "draw: pane flushed {} bytes in {:?}",
                    r.pane_bytes,
                    r.last_pane_bytes_log.elapsed()
                );
                r.pane_bytes = 0;
                r.last_pane_bytes_log = Instant::now();
            }
        }
        r.dirty = false;
        r.full_redraw = false;
        r.last_clock_redraw = Instant::now();

        // Terminal round-trip probe (see `probe_ok` above): fired only
        // right after a keypress-triggered frame, at most once per
        // PROBE_INTERVAL. A slow answer here with a fast `latency=` on
        // the same frame localizes the felt lag to the terminal side.
        if r.probe_ok && input_latency.is_some() && r.last_probe.elapsed() >= PROBE_INTERVAL {
            r.last_probe = Instant::now();
            let t_probe = Instant::now();
            match crossterm::cursor::position() {
                Ok(_) => {
                    let rtt = t_probe.elapsed();
                    if rtt >= Duration::from_millis(100) {
                        log::warn!(
                            "probe: terminal answered CPR in {:?} — terminal-side backlog",
                            rtt
                        );
                    } else {
                        log::debug!("probe: terminal CPR rtt={:?}", rtt);
                    }
                }
                Err(e) => {
                    r.probe_ok = false;
                    log::warn!("probe: CPR failed ({}); probing disabled for this run", e);
                }
            }
        }
    }
    Ok(draw_dur)
}

/// Replay clipboard escapes (OSC 52) captured from the embedded pane
/// onto the real terminal. tmux addresses the escape to its attach
/// client — the pane's pty, where the vt100 parser would drop it —
/// so this hop is what carries an in-pane copy to the viewer's
/// terminal when cc-hub runs on a remote box over ssh. Done here,
/// between frames on the render thread, so a replay can never tear
/// a ratatui write.
pub(super) fn replay_osc52(terminal: &mut Term, app: &App) {
    if let Some(pane) = app.tmux_pane.as_ref() {
        for seq in pane.take_osc52() {
            let backend = terminal.backend_mut();
            if let Err(e) = backend.write_all(&seq).and_then(|()| backend.flush()) {
                log::warn!("osc52 replay failed: {}", e);
            }
        }
    }
}
