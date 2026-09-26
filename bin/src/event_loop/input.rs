use super::render::Redraw;
use crate::keys;
use crate::scan_msg::ScanMsg;
use crate::term::Term;
use cc_hub_lib::app::{App, View};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::io;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// How long the window must go without a Resize before the second full
/// repaint.
const RESIZE_SETTLE: Duration = Duration::from_millis(250);

/// Wait briefly for input, then handle the whole burst that arrived. Returns
/// the time spent handling it.
pub(super) async fn drain_input(
    app: &mut App,
    terminal: &mut Term,
    scan_tx: &mpsc::Sender<ScanMsg>,
    detail_tx: &mpsc::Sender<String>,
    r: &mut Redraw,
) -> io::Result<Duration> {
    let poll_ms = if app.view == View::TmuxPane { 16 } else { 5 };

    let mut input_dur = Duration::ZERO;
    if event::poll(Duration::from_millis(poll_ms))? {
        let t_input = Instant::now();
        // Drain the whole input burst before the next draw. Reading one
        // event per loop pass meant every queued keystroke paid a full
        // render + scan drain before the next was even looked at — fast
        // typing (or a mouse-move flood in the pane) backlogged and read
        // as "my key was never processed". Bounded so a pathological
        // event stream can't starve rendering entirely; leftovers are
        // picked up by the next pass's poll() immediately.
        for _ in 0..64 {
            let evt = event::read()?;
            // Any input may mutate state (scroll, selection, view change,
            // status line) — repaint after the burst regardless of which
            // arm handles it.
            r.dirty = true;
            r.last_input_at = Some(Instant::now());
            match evt {
                Event::Mouse(m) => {
                    if app.view == View::TmuxPane {
                        if let Some(pane) = app.tmux_pane.as_mut() {
                            pane.send_mouse(m);
                        }
                    }
                }
                Event::Paste(text) => {
                    if app.view == View::TmuxPane {
                        if let Some(pane) = app.tmux_pane.as_ref() {
                            if let Err(e) = pane.paste_text(&text) {
                                app.set_status(format!("paste failed: {}", e));
                            }
                        }
                    } else {
                        // Single-line task inputs (add/rename, tags,
                        // attach) accept pastes — without this, bracketed
                        // paste swallowed the burst and pasting a path
                        // into the attach popup did nothing.
                        app.paste_into_input(&text);
                    }
                }
                Event::Key(key) => {
                    // Trace every key so a "my press did nothing" report
                    // can be answered from the log: did it arrive, with
                    // what kind, and which view received it.
                    log::debug!(
                        "key: {:?} kind={:?} mods={:?} view={:?} tab={:?}",
                        key.code,
                        key.kind,
                        key.modifiers,
                        app.view,
                        app.current_tab
                    );
                    // Ctrl+L: classic manual full repaint, and a live
                    // diagnostic — if a "stuck" highlight snaps right
                    // after this, the physical screen had diverged from
                    // ratatui's buffer. Intercepted here because keys.rs
                    // reads a bare Char('l') as move-right; inside the
                    // pane it falls through to the shell.
                    //
                    // Nothing in this arm uses an early `continue`: it
                    // would skip the poll(0) burst check below and block
                    // the next read() on an empty queue.
                    let force_redraw = key.code == KeyCode::Char('l')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                        && app.view != View::TmuxPane;
                    if force_redraw {
                        log::debug!("key: ctrl+l — manual full redraw");
                        terminal.clear()?;
                    } else if key.kind != KeyEventKind::Release {
                        // Repeat is routed like Press — kitty (we push
                        // DISAMBIGUATE at startup) tags held-key repeats
                        // as Repeat, and the old `!= Press` filter dropped
                        // every one. Release alone stays ignored.
                        //
                        // The pane child can exit while we sit in poll();
                        // the loop-top cleanup hasn't run yet, so without
                        // this re-check the key would be written into a
                        // dead pty and vanish. Close first, route normally.
                        if app.view == View::TmuxPane
                            && app.tmux_pane.as_ref().is_some_and(|p| p.is_exited())
                        {
                            app.close_tmux_pane();
                        }
                        let sel_before = (app.sessions.sel_group, app.sessions.sel_in_group);
                        keys::handle_key(app, key, terminal, scan_tx, detail_tx).await;
                        let sel_after = (app.sessions.sel_group, app.sessions.sel_in_group);
                        if sel_before != sel_after {
                            log::debug!("key: selection {:?} -> {:?}", sel_before, sel_after);
                        }
                    }
                }
                Event::Resize(w, h) => {
                    // Never trust `autoresize` alone here — see the
                    // `full_redraw` comment. Every resize repaints in
                    // full now, and once more after the storm settles.
                    log::debug!("event: Resize({}, {})", w, h);
                    r.full_redraw = true;
                    r.resize_settle_at = Some(Instant::now() + RESIZE_SETTLE);
                }
                Event::FocusGained => {
                    // Coming back to the window: whatever the terminal
                    // did to the screen while we were hidden, one full
                    // frame is cheap insurance.
                    log::debug!("event: FocusGained — full redraw");
                    r.full_redraw = true;
                }
                other => {
                    // FocusLost: nothing to do beyond the repaint, but
                    // keep it visible in the trace.
                    log::debug!("event: {:?}", other);
                }
            }
            if app.should_quit || !event::poll(Duration::ZERO)? {
                break;
            }
        }
        input_dur = t_input.elapsed();
    }
    Ok(input_dur)
}
