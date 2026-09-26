//! The TUI's single-threaded draw/input loop.
//!
//! - `render`: redraw gating, the draw trace, the terminal round-trip probe
//!   and OSC 52 replay.
//! - `input`: drains one input burst and routes keys to [`crate::keys`].

mod input;
mod render;

use crate::scan_msg::{apply_scan_msg, ScanMsg};
use crate::term::Term;
use crate::titles::Titles;
use crate::{logging, workers};
use cc_hub_lib::app::{self, App, View};
use cc_hub_lib::config;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use std::io;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Any single loop phase taking this long is a responsiveness incident
/// worth a warn-level breakdown in the log.
const STALL: Duration = Duration::from_millis(30);

pub(crate) async fn run(terminal: &mut Term, frame_bytes: Arc<AtomicU64>) -> io::Result<()> {
    let mut app = App::new();
    // Swap in the persisted ack tracker so Space-idled cards survive a
    // restart. Loaded here, not in App::new(): tests must never touch the
    // real home, so App::new() constructs a purely in-memory tracker.
    app.sessions.acks = cc_hub_lib::acks::Acks::load();

    let titles = Titles::new(config::get().title.concurrency);

    let (scan_tx, mut scan_rx) = mpsc::channel::<ScanMsg>(16);
    let (detail_tx, detail_rx) = mpsc::channel::<String>(4);

    workers::spawn_usage(scan_tx.clone());
    app.harness.supervisor_on = workers::spawn_harness(scan_tx.clone());
    workers::spawn_builds(&scan_tx);
    workers::spawn_session_scanner(scan_tx.clone(), detail_rx);
    workers::spawn_task_stats(scan_tx.clone());

    // Capture only while the embedded tmux pane is visible so the host
    // terminal's native wheel scroll keeps working elsewhere.
    let mut mouse_captured = false;
    let mut redraw = render::Redraw::new();
    let mut last_sys_log = Instant::now();

    loop {
        // Poll live view for new JSONL entries
        if app.view == View::LiveTail {
            if let Some(ref mut lv) = app.live_view {
                if lv.poll() {
                    redraw.dirty = true;
                }
            }
        }

        if app.view == View::TmuxPane && app.tmux_pane.as_ref().is_some_and(|p| p.is_exited()) {
            app.close_tmux_pane();
            redraw.dirty = true;
        }

        let want_mouse = app.view == View::TmuxPane;
        if want_mouse != mouse_captured {
            let backend = terminal.backend_mut();
            let res = if want_mouse {
                crossterm::execute!(backend, EnableMouseCapture)
            } else {
                crossterm::execute!(backend, DisableMouseCapture)
            };
            match res {
                Ok(()) => mouse_captured = want_mouse,
                Err(e) => log::warn!("mouse capture toggle failed: {}", e),
            }
            redraw.dirty = true;
        }

        let draw_dur = render::draw(terminal, &mut app, &frame_bytes, &mut redraw)?;
        render::replay_osc52(terminal, &app);

        let input_dur =
            input::drain_input(&mut app, terminal, &scan_tx, &detail_tx, &mut redraw).await?;

        // Drain channel messages. Repaint only when a message actually
        // changed visible state — the periodic scan ticks usually carry an
        // identical snapshot, and skipping those keeps an unchanged grid
        // (and its selection) untouched between real changes.
        let t_drain = Instant::now();
        while let Ok(msg) = scan_rx.try_recv() {
            if apply_scan_msg(&mut app, msg, &titles) {
                redraw.dirty = true;
            }
        }
        let drain_dur = t_drain.elapsed();

        // If a prompt was queued for an auto-spawned session, send it once the
        // session reports Idle in the latest scan.
        let t_dispatch = Instant::now();
        match app.poll_pending_dispatch() {
            app::DispatchAction::Send { tmux, prompt } => {
                log::info!(
                    "dispatch: pending target [{}] now idle, sending (len={})",
                    tmux,
                    prompt.len()
                );
                workers::spawn_dispatch(
                    scan_tx.clone(),
                    tmux.clone(),
                    prompt,
                    format!("dispatched queued prompt to [{}]", tmux),
                    "queued dispatch failed".to_string(),
                );
                redraw.dirty = true;
            }
            app::DispatchAction::Timeout { tmux } => {
                log::warn!("dispatch: pending target [{}] never became idle", tmux);
                app.set_status(format!(
                    "queued dispatch timed out — [{}] never became idle",
                    tmux
                ));
                redraw.dirty = true;
            }
            app::DispatchAction::Wait => {}
        }
        let dispatch_dur = t_dispatch.elapsed();

        // Periodic machine-load line: felt lag with clean loop phases and a
        // clean probe points at the OS/emulator being starved — this ties
        // each incident window to the load averages at that moment.
        if last_sys_log.elapsed() >= Duration::from_secs(10) {
            last_sys_log = Instant::now();
            logging::log_loadavg();
        }

        // Self-profiling: any phase that held the loop past STALL is exactly
        // the kind of incident users report as "input lag" — name it with
        // numbers instead of leaving it to feel.
        if draw_dur >= STALL || input_dur >= STALL || drain_dur >= STALL || dispatch_dur >= STALL {
            log::warn!(
                "loop stall: draw={:?} input={:?} drain={:?} dispatch={:?}",
                draw_dur,
                input_dur,
                drain_dur,
                dispatch_dur
            );
        }

        if app.should_quit {
            app.log_state_dump();
            break;
        }
    }

    Ok(())
}
