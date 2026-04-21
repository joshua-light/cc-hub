use cc_hub_lib::{
    app, clipboard, config, conversation, focus, gh, live_view, metrics, models, platform,
    scanner, send, spawn, title, tmux_pane, ui, usage, watcher,
};

use app::{App, Tab, View};

#[cfg(feature = "hot-reload")]
#[hot_lib_reloader::hot_module(dylib = "cc_hub_lib", lib_dir = "target/debug")]
mod hot {
    use cc_hub_lib::app;
    use ratatui::Frame;
    hot_functions_from_file!("lib/src/lib.rs");
}

#[cfg(not(feature = "hot-reload"))]
mod hot {
    pub use cc_hub_lib::render;
}
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use log::LevelFilter;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use simplelog::{Config as LogConfig, WriteLogger};
use std::collections::HashSet;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

/// Spawn a background `cc-hub-new -p` per session that has a first user
/// message but no cached title yet. `inflight` guards against duplicate
/// kickoffs (and intentionally retains failed sids so a missing shell
/// command doesn't trigger a subprocess every scan). `active` is the
/// narrower set of sids whose subprocess is actually running right now,
/// and drives the UI spinner.
fn queue_missing_titles(
    sessions: &mut [models::SessionInfo],
    inflight: &Arc<Mutex<HashSet<String>>>,
    active: &Arc<Mutex<HashSet<String>>>,
    gate: &Arc<tokio::sync::Semaphore>,
) {
    for session in sessions.iter() {
        if session.title.is_some() {
            continue;
        }
        let Some(first_msg) = session.summary.clone() else {
            continue;
        };
        let sid = session.session_id.clone();
        {
            let mut lock = inflight.lock().unwrap_or_else(|e| e.into_inner());
            if !lock.insert(sid.clone()) {
                continue; // already being titled
            }
        }
        let inflight = Arc::clone(inflight);
        let active = Arc::clone(active);
        let gate = Arc::clone(gate);
        tokio::spawn(async move {
            // Hold the permit across the blocking subprocess call so only
            // `TITLE_CONCURRENCY` children ever exist at once. The permit
            // drops at task end, freeing a slot for the next queued title.
            let _permit = gate.acquire_owned().await.ok();

            // Mark active only around the real work — the UI spinner is
            // driven by this narrower set, so a pending task still gated
            // on the semaphore doesn't flash ✎ on its card.
            active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(sid.clone());

            let title_result = tokio::task::spawn_blocking({
                let msg = first_msg.clone();
                move || title::generate_title_blocking(&msg)
            })
            .await
            .ok()
            .flatten();

            active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&sid);

            // Leaving a failed sid in `inflight` blocks retry for this
            // process's lifetime — the intended behavior when the user's
            // shell is missing `cc-hub-new` entirely, so we don't spawn
            // a failing subprocess every scan tick. An app restart clears
            // the set and retries naturally.
            let Some(t) = title_result else {
                log::warn!(
                    "title: generation failed for {}, blocking retries this session",
                    models::short_sid(&sid)
                );
                return;
            };

            let sid_for_persist = sid.clone();
            let t_for_persist = t.clone();
            let persist = tokio::task::spawn_blocking(move || {
                title::persist_title(&sid_for_persist, &t_for_persist)
            })
            .await;
            match persist {
                Ok(Ok(())) => log::info!(
                    "title: sid={} → {:?}",
                    models::short_sid(&sid),
                    t
                ),
                Ok(Err(e)) => log::warn!("title: persist failed for {}: {}", sid, e),
                Err(e) => log::warn!("title: persist task panicked for {}: {}", sid, e),
            }

            // On success we drop the sid so the set doesn't grow unboundedly
            // across many sessions; subsequent scans see the cached title
            // and skip re-queueing anyway.
            inflight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&sid);
        });
    }

    // Second pass: stamp titling on every session whose sid is currently
    // running (not just queued). Read the active set *after* insertion
    // races above so UI sees the same instant the subprocess starts. The
    // "queued but gated" window where a permit is still pending shows up
    // as no indicator — that's brief and indistinguishable from "about to
    // start" anyway.
    let set = active.lock().unwrap_or_else(|e| e.into_inner());
    for session in sessions.iter_mut() {
        session.titling = set.contains(&session.session_id);
    }
}

enum ScanMsg {
    SessionList(Vec<models::SessionInfo>),
    Detail(models::SessionDetail),
    StateDebug(models::SessionInfo, conversation::StateExplanation),
    Usage(usage::UsageInfo),
    Metrics(metrics::MetricsAnalysis),
    MetricsProgress {
        scanned: usize,
        total: usize,
    },
    GhCreateDone {
        name: String,
        result: Result<String, String>,
    },
}

/// Size for a popup tmux pane: terminal minus a margin, with floor. The
/// renderer re-resizes on first draw, so a rough starting size is fine.
fn popup_pane_size(terminal: &Terminal<CrosstermBackend<io::Stdout>>) -> (u16, u16) {
    terminal
        .size()
        .map(|s| (s.width.saturating_sub(6).max(20), s.height.saturating_sub(6).max(10)))
        .unwrap_or((120, 30))
}

fn init_logging() -> PathBuf {
    let log_dir = platform::paths::cache_dir();
    std::fs::create_dir_all(&log_dir).ok();

    let log_path = log_dir.join(format!(
        "cc-hub_{}.log",
        chrono::Local::now().format("%Y%m%d_%H%M%S")
    ));

    if let Ok(file) = File::create(&log_path) {
        WriteLogger::init(LevelFilter::Debug, LogConfig::default(), file).ok();
    }

    log_path
}

fn restore_terminal<W: io::Write>(
    out: &mut W,
    bracketed_paste: bool,
    kb_enhanced: bool,
) -> io::Result<()> {
    let _ = crossterm::execute!(out, DisableMouseCapture);
    if bracketed_paste {
        let _ = crossterm::execute!(out, DisableBracketedPaste);
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
        let _ = restore_terminal(&mut out, true, true);
        prev(info);
    }));
}

#[tokio::main]
async fn main() -> io::Result<()> {
    if std::env::args().any(|a| a == "--no-tui") {
        return run_no_tui();
    }

    let log_path = init_logging();

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
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal).await;

    // Ask any still-running title subprocesses to kill themselves so the
    // tokio runtime's shutdown doesn't wait up to ~45s on a hung `claude
    // -p`. Blocking tasks can't be cancelled, but they poll this flag.
    title::request_shutdown();

    restore_terminal(terminal.backend_mut(), bracketed_paste, kb_enhanced)?;
    terminal.show_cursor()?;

    eprintln!("Logs: {}", log_path.display());

    result
}

fn run_no_tui() -> io::Result<()> {
    let sessions = scanner::scan_sessions();
    for s in &sessions {
        let last_msg = s.last_user_message.as_deref().unwrap_or("");
        println!(
            "{:>7}:{} [{:<17}] {:<24} {}",
            s.pid,
            models::short_sid(&s.session_id),
            s.state,
            s.project_name,
            last_msg
        );
    }
    println!("— {} sessions —", sessions.len());
    Ok(())
}

async fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let mut app = App::new();

    let inflight_titles: Arc<Mutex<HashSet<String>>> =
        Arc::new(Mutex::new(HashSet::new()));
    let active_titles: Arc<Mutex<HashSet<String>>> =
        Arc::new(Mutex::new(HashSet::new()));
    let title_gate: Arc<tokio::sync::Semaphore> =
        Arc::new(tokio::sync::Semaphore::new(config::get().title.concurrency));

    let (scan_tx, mut scan_rx) = mpsc::channel::<ScanMsg>(16);
    let (detail_tx, mut detail_rx) = mpsc::channel::<String>(4);
    let (state_debug_tx, mut state_debug_rx) = mpsc::channel::<String>(4);

    let usage_tx = scan_tx.clone();
    let scan_tx_main = scan_tx.clone();
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(config::get().scan.usage_refresh_interval());
        loop {
            interval.tick().await;
            if let Some(u) = tokio::task::spawn_blocking(usage::fetch_usage)
                .await
                .ok()
                .flatten()
            {
                let _ = usage_tx.send(ScanMsg::Usage(u)).await;
            }
        }
    });

    // Fallback timer catches PID deaths (not a filesystem event) and events
    // missed when a watched dir is rotated or recreated. Its initial tick
    // fires immediately, serving as the startup scan.
    let (fs_tx, mut fs_rx) = mpsc::channel::<()>(8);
    watcher::spawn_fs_watcher(fs_tx);

    tokio::spawn(async move {
        let mut fallback =
            tokio::time::interval(config::get().scan.fs_fallback_interval());
        fallback.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut latest_sessions: Vec<models::SessionInfo> = Vec::new();

        loop {
            tokio::select! {
                _ = fallback.tick() => {
                    let sessions = tokio::task::spawn_blocking(scanner::scan_sessions)
                        .await
                        .unwrap_or_default();
                    latest_sessions = sessions.clone();
                    let _ = scan_tx.send(ScanMsg::SessionList(sessions)).await;
                }
                Some(()) = fs_rx.recv() => {
                    // Drain coalesced signals — one scan per burst is enough.
                    while fs_rx.try_recv().is_ok() {}
                    let sessions = tokio::task::spawn_blocking(scanner::scan_sessions)
                        .await
                        .unwrap_or_default();
                    latest_sessions = sessions.clone();
                    let _ = scan_tx.send(ScanMsg::SessionList(sessions)).await;
                }
                Some(session_id) = detail_rx.recv() => {
                    let sessions = latest_sessions.clone();
                    let detail = tokio::task::spawn_blocking(move || {
                        scanner::load_detail(&session_id, &sessions)
                    })
                    .await
                    .ok()
                    .flatten();
                    if let Some(d) = detail {
                        let _ = scan_tx.send(ScanMsg::Detail(d)).await;
                    }
                }
                Some(session_id) = state_debug_rx.recv() => {
                    let sessions = latest_sessions.clone();
                    let exp = tokio::task::spawn_blocking(move || {
                        scanner::load_state_explanation(&session_id, &sessions)
                    })
                    .await
                    .ok()
                    .flatten();
                    if let Some((info, e)) = exp {
                        let _ = scan_tx.send(ScanMsg::StateDebug(info, e)).await;
                    }
                }
            }
        }
    });

    let spawn_metrics = || {
        let tx = scan_tx_main.clone();
        tokio::spawn(async move {
            let progress_tx = tx.clone();
            let fut = tokio::task::spawn_blocking(move || {
                // Throttle progress updates: the scanner rips through
                // several hundred files per second on warm cache, so report
                // at most every ~20 files (plus the 0 and N boundaries) to
                // keep the 16-slot channel from ever filling.
                let mut last_sent: usize = 0;
                metrics::analyze_with_progress(|scanned, total| {
                    let at_edge = scanned == 0 || scanned == total;
                    if at_edge || scanned.saturating_sub(last_sent) >= 20 {
                        last_sent = scanned;
                        let _ = progress_tx
                            .try_send(ScanMsg::MetricsProgress { scanned, total });
                    }
                })
            });
            if let Ok(m) = fut.await {
                let _ = tx.send(ScanMsg::Metrics(m)).await;
            }
        });
    };

    // Capture only while the embedded tmux pane is visible so the host
    // terminal's native wheel scroll keeps working elsewhere.
    let mut mouse_captured = false;

    loop {
        // Poll live view for new JSONL entries
        if app.view == View::LiveTail {
            if let Some(ref mut lv) = app.live_view {
                lv.poll();
            }
        }

        if app.view == View::TmuxPane
            && app.tmux_pane.as_ref().is_some_and(|p| p.is_exited())
        {
            app.close_tmux_pane();
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
        }

        terminal.draw(|frame| hot::render(frame, &mut app))?;

        let poll_ms = if app.view == View::TmuxPane { 16 } else { 50 };

        if event::poll(Duration::from_millis(poll_ms))? {
            let evt = event::read()?;
            if let Event::Mouse(m) = evt {
                if app.view == View::TmuxPane {
                    if let Some(pane) = app.tmux_pane.as_mut() {
                        pane.send_mouse(m);
                    }
                }
                continue;
            }
            if let Event::Paste(text) = evt {
                if app.view == View::TmuxPane {
                    if let Some(pane) = app.tmux_pane.as_ref() {
                        if let Err(e) = pane.paste_text(&text) {
                            app.set_status(format!("paste failed: {}", e));
                        }
                    }
                }
                continue;
            }
            if let Event::Key(key) = evt {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let on_sessions = app.view == View::Grid && app.current_tab == Tab::Sessions;
                let on_metrics = app.view == View::Grid && app.current_tab == Tab::Metrics;

                match (&app.view, key.code) {
                    // Quit
                    (View::Grid, KeyCode::Char('q')) => {
                        app.should_quit = true;
                    }
                    (View::Grid, KeyCode::Tab | KeyCode::BackTab) => {
                        let was_metrics = app.current_tab == Tab::Metrics;
                        app.cycle_tab();
                        if !was_metrics
                            && app.current_tab == Tab::Metrics
                            && app.metrics.is_none()
                        {
                            spawn_metrics();
                        }
                    }
                    (View::Grid, KeyCode::Char('m')) if on_sessions => {
                        let needs_compute = app.metrics.is_none();
                        app.set_tab(Tab::Metrics);
                        if needs_compute {
                            spawn_metrics();
                        }
                    }
                    (View::Grid, KeyCode::Right | KeyCode::Char('l')) if on_sessions => {
                        app.move_right()
                    }
                    (View::Grid, KeyCode::Left | KeyCode::Char('h')) if on_sessions => {
                        app.move_left()
                    }
                    (View::Grid, KeyCode::Down | KeyCode::Char('j')) if on_sessions => {
                        app.move_down()
                    }
                    (View::Grid, KeyCode::Up | KeyCode::Char('k')) if on_sessions => {
                        app.move_up()
                    }
                    (View::Grid, KeyCode::Down | KeyCode::Char('j')) if on_metrics => {
                        if app.metrics_rows.is_empty() {
                            app.metrics_scroll_down();
                        } else {
                            app.metrics_sel_next();
                        }
                    }
                    (View::Grid, KeyCode::Up | KeyCode::Char('k')) if on_metrics => {
                        if app.metrics_rows.is_empty() {
                            app.metrics_scroll_up();
                        } else {
                            app.metrics_sel_prev();
                        }
                    }
                    (View::Grid, KeyCode::Enter) if on_metrics => {
                        if let Some(row) = app.selected_metrics_session().cloned() {
                            let lv = live_view::LiveView::review(
                                row.jsonl_path.clone(),
                                row.peak_timestamp_ms,
                            );
                            if lv.messages.is_empty() {
                                app.set_status(format!(
                                    "can't open {}: {} missing or empty",
                                    models::short_sid(&row.session_id),
                                    row.jsonl_path.display()
                                ));
                            } else {
                                app.enter_live_tail(lv);
                            }
                        }
                    }
                    (View::Grid, KeyCode::Char('r')) if on_metrics => {
                        app.metrics = None;
                        spawn_metrics();
                    }
                    // 'i' for info popup (old Enter behavior)
                    (View::Grid, KeyCode::Char('i')) if on_sessions => {
                        if let Some(id) = app.selected_session_id() {
                            let _ = detail_tx.send(id).await;
                            app.enter_popup();
                        }
                    }
                    (View::Grid, KeyCode::Char('D')) if on_sessions => {
                        if let Some(id) = app.selected_session_id() {
                            let _ = state_debug_tx.send(id).await;
                            app.enter_state_debug();
                        }
                    }
                    (View::StateDebug, KeyCode::Esc | KeyCode::Char('q')) => {
                        app.close_state_debug();
                    }
                    (View::StateDebug, KeyCode::Down | KeyCode::Char('j')) => {
                        app.debug_scroll_down();
                    }
                    (View::StateDebug, KeyCode::Up | KeyCode::Char('k')) => {
                        app.debug_scroll_up();
                    }
                    (View::Grid, KeyCode::Char('H')) if on_sessions => {
                        app.toggle_show_inactive();
                        let state = if app.show_inactive { "shown" } else { "hidden" };
                        app.set_status(format!("inactive sessions {}", state));
                    }
                    (View::Grid, KeyCode::Char('f') | KeyCode::Enter) if on_sessions => {
                        if let Some(session) = app.selected_session_info().cloned() {
                            if session.state == models::SessionState::Inactive {
                                let status = match spawn::spawn_claude_session(
                                    &session.cwd,
                                    Some(&session.session_id),
                                ) {
                                    Ok(name) => format!(
                                        "resumed {} [{}]",
                                        models::short_sid(&session.session_id),
                                        name
                                    ),
                                    Err(e) => format!("resume failed: {}", e),
                                };
                                app.set_status(status);
                            } else if let Some(tmux_name) = session.tmux_session.clone() {
                                let (cols, rows) = popup_pane_size(terminal);
                                match tmux_pane::TmuxPaneView::spawn(&tmux_name, rows, cols) {
                                    Ok(pane) => {
                                        app.enter_tmux_pane(pane);
                                    }
                                    Err(e) => {
                                        app.set_status(format!("tmux attach failed: {}", e));
                                    }
                                }
                            } else {
                                match focus::focus_window(session.pid) {
                                    focus::FocusOutcome::Focused => {}
                                    focus::FocusOutcome::NeedsReattach(name) => {
                                        let msg = match spawn::attach_tmux_session(&name, &session.cwd) {
                                            Ok(_) => format!("reattached terminal to {}", name),
                                            Err(e) => format!("reattach failed: {}", e),
                                        };
                                        app.set_status(msg);
                                    }
                                    focus::FocusOutcome::Failed(msg) => {
                                        app.set_status(msg);
                                    }
                                }
                            }
                        }
                    }
                    (View::Grid, KeyCode::Char('o')) if on_sessions => {
                        if let Some(session) = app.selected_session_info().cloned() {
                            let (cols, rows) = popup_pane_size(terminal);
                            match spawn::spawn_shell_tmux_session(&session.cwd) {
                                Ok(tmux_name) => match tmux_pane::TmuxPaneView::spawn_owned(&tmux_name, rows, cols) {
                                    Ok(pane) => app.enter_tmux_pane(pane),
                                    Err(e) => app.set_status(format!("shell attach failed: {}", e)),
                                },
                                Err(e) => {
                                    app.set_status(format!("shell spawn failed: {}", e));
                                }
                            }
                        }
                    }
                    (View::TmuxPane, KeyCode::F(1)) => {
                        app.close_tmux_pane();
                    }
                    (View::TmuxPane, KeyCode::Char(c))
                        if (c == 'v' || c == 'V')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                            && key.modifiers.contains(KeyModifiers::SHIFT) =>
                    {
                        let status = match clipboard::paste() {
                            Ok(text) if text.is_empty() => Some("clipboard empty".to_string()),
                            Ok(text) => match app.tmux_pane.as_ref() {
                                Some(pane) => pane
                                    .paste_text(&text)
                                    .err()
                                    .map(|e| format!("paste failed: {}", e)),
                                None => None,
                            },
                            Err(e) => Some(format!("paste failed: {}", e)),
                        };
                        if let Some(msg) = status {
                            app.set_status(msg);
                        }
                    }
                    (View::TmuxPane, _) => {
                        if let Some(pane) = app.tmux_pane.as_mut() {
                            pane.send_key(key);
                        }
                    }
                    (View::Grid, KeyCode::Char('x')) if on_sessions => {
                        app.enter_confirm_close();
                    }
                    (View::ConfirmClose, KeyCode::Char('y') | KeyCode::Char('Y')) => {
                        if let Some(pending) = app.take_pending_close() {
                            let ok = focus::close_window(pending.pid);
                            let msg = if ok {
                                format!("closed {}", pending.display)
                            } else {
                                format!("failed to close {}", pending.display)
                            };
                            app.set_status(msg);
                        }
                    }
                    (
                        View::ConfirmClose,
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q'),
                    ) => {
                        app.cancel_confirm_close();
                    }
                    // Space: force selected session to display as Idle until new
                    // activity advances its watermark.
                    (View::Grid, KeyCode::Char(' ')) if on_sessions => {
                        app.ack_selected();
                    }
                    (View::Grid, KeyCode::Char('n')) if on_sessions => {
                        if let Some(sess) = app.selected_session_info().cloned() {
                            let status = match spawn::spawn_claude_session(&sess.cwd, None) {
                                Ok(name) => format!("started cc-hub-new [{}]", name),
                                Err(e) => format!("spawn failed: {}", e),
                            };
                            app.set_status(status);
                        }
                    }
                    (View::Grid, KeyCode::Char('N')) if on_sessions => {
                        app.enter_folder_picker();
                    }
                    (View::FolderPicker, KeyCode::Esc | KeyCode::Char('q')) => {
                        app.close_folder_picker();
                    }
                    (View::FolderPicker, KeyCode::Down | KeyCode::Char('j')) => {
                        if let Some(p) = app.folder_picker.as_mut() {
                            p.move_down();
                        }
                    }
                    (View::FolderPicker, KeyCode::Up | KeyCode::Char('k')) => {
                        if let Some(p) = app.folder_picker.as_mut() {
                            p.move_up();
                        }
                    }
                    (View::FolderPicker, KeyCode::Enter | KeyCode::Right | KeyCode::Char('l')) => {
                        if let Some(p) = app.folder_picker.as_mut() {
                            p.descend();
                        }
                    }
                    (View::FolderPicker, KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h')) => {
                        if let Some(p) = app.folder_picker.as_mut() {
                            p.ascend();
                        }
                    }
                    (View::FolderPicker, KeyCode::Char(' ')) => {
                        let cwd = app.folder_picker.as_ref().and_then(|p| {
                            p.entries
                                .get(p.selection)
                                .map(|name| p.current_dir.join(name).display().to_string())
                        });
                        app.close_folder_picker();
                        if let Some(cwd) = cwd {
                            let status = match spawn::spawn_claude_session(&cwd, None) {
                                Ok(name) => format!("started cc-hub-new [{}]", name),
                                Err(e) => format!("spawn failed: {}", e),
                            };
                            app.set_status(status);
                        }
                    }
                    (View::FolderPicker, KeyCode::Char('.')) => {
                        let cwd = app
                            .folder_picker
                            .as_ref()
                            .map(|p| p.current_dir.display().to_string());
                        app.close_folder_picker();
                        if let Some(cwd) = cwd {
                            let status = match spawn::spawn_claude_session(&cwd, None) {
                                Ok(name) => format!("started cc-hub-new [{}]", name),
                                Err(e) => format!("spawn failed: {}", e),
                            };
                            app.set_status(status);
                        }
                    }
                    (View::FolderPicker, KeyCode::Char('c')) => {
                        app.enter_gh_create_input(false);
                    }
                    (View::FolderPicker, KeyCode::Char('C')) => {
                        app.enter_gh_create_input(true);
                    }
                    (View::GhCreateInput, KeyCode::Esc) => {
                        app.close_gh_create_input();
                    }
                    (View::GhCreateInput, KeyCode::Tab) => {
                        if let Some(input) = app.gh_create_input.as_mut() {
                            input.private = !input.private;
                        }
                    }
                    (View::GhCreateInput, KeyCode::Backspace) => {
                        if let Some(input) = app.gh_create_input.as_mut() {
                            input.name.pop();
                        }
                    }
                    (View::GhCreateInput, KeyCode::Char(c)) => {
                        if let Some(input) = app.gh_create_input.as_mut() {
                            input.name.push(c);
                        }
                    }
                    (View::GhCreateInput, KeyCode::Enter) => {
                        let name_empty = app
                            .gh_create_input
                            .as_ref()
                            .is_none_or(|i| i.name.trim().is_empty());
                        if name_empty {
                            app.set_status("repo name cannot be empty".into());
                            continue;
                        }
                        if let Some((cwd, name, private)) = app.submit_gh_create_input() {
                            let trimmed = name.trim().to_string();
                            let tx = scan_tx_main.clone();
                            let name_for_msg = trimmed.clone();
                            tokio::spawn(async move {
                                let result = tokio::task::spawn_blocking(move || {
                                    gh::create_repo(&cwd, &trimmed, private)
                                })
                                .await
                                .unwrap_or_else(|e| Err(format!("task panicked: {}", e)));
                                let _ = tx
                                    .send(ScanMsg::GhCreateDone {
                                        name: name_for_msg,
                                        result,
                                    })
                                    .await;
                            });
                            app.set_status(format!(
                                "creating {} repo {}…",
                                if private { "private" } else { "public" },
                                name
                            ));
                        }
                    }
                    (View::Grid, KeyCode::Char('p')) if on_sessions => {
                        app.enter_prompt_input();
                    }
                    (View::PromptInput, KeyCode::Esc) => {
                        app.close_prompt_input();
                    }
                    (View::PromptInput, KeyCode::Backspace) => {
                        app.prompt_buffer.pop();
                    }
                    (View::PromptInput, KeyCode::Char(c)) => {
                        app.prompt_buffer.push(c);
                    }
                    (View::PromptInput, KeyCode::Enter) => {
                        let target = app.dispatch_target().cloned();
                        if app.prompt_buffer.trim().is_empty() {
                            app.close_prompt_input();
                            app.set_status("empty prompt — dispatch cancelled".into());
                            continue;
                        }
                        let prompt = app.submit_prompt_input();

                        if let Some((pid, name, tmux)) = target {
                            log::info!(
                                "dispatch: idle target {} (PID {}) [{}] prompt_len={}",
                                name, pid, tmux, prompt.len()
                            );
                            let status = match send::send_prompt(&tmux, &prompt) {
                                Ok(()) => format!("dispatched to {} (PID {}) [{}]", name, pid, tmux),
                                Err(e) => {
                                    log::warn!("dispatch: send_prompt failed: {}", e);
                                    format!("dispatch failed: {}", e)
                                }
                            };
                            app.set_status(status);
                            continue;
                        }

                        if app.has_pending_dispatch() {
                            app.set_status(
                                "dispatch already pending — wait for the new agent to come up".into(),
                            );
                            continue;
                        }
                        let Some(cwd) = app.default_spawn_cwd() else {
                            app.set_status("no idle agent and no cwd to spawn in".into());
                            continue;
                        };
                        match spawn::spawn_claude_session(&cwd, None) {
                            Ok(tmux_name) => {
                                log::info!(
                                    "dispatch: no idle agent, spawned [{}] in {} — queueing prompt (len={})",
                                    tmux_name, cwd, prompt.len()
                                );
                                app.queue_pending_dispatch(tmux_name.clone(), prompt);
                                app.set_status(format!(
                                    "no idle agent — spawned [{}], prompt queued",
                                    tmux_name
                                ));
                            }
                            Err(e) => {
                                log::warn!("dispatch: auto-spawn failed: {}", e);
                                app.set_status(format!("auto-spawn failed: {}", e));
                            }
                        }
                    }
                    // Popup navigation
                    (View::Popup, KeyCode::Esc | KeyCode::Char('q')) => app.close_popup(),
                    (View::Popup, KeyCode::Down | KeyCode::Char('j')) => app.scroll_down(),
                    (View::Popup, KeyCode::Up | KeyCode::Char('k')) => app.scroll_up(),
                    // Live tail view
                    (View::LiveTail, KeyCode::Esc | KeyCode::Char('q')) => {
                        app.close_live_tail();
                    }
                    (View::LiveTail, KeyCode::Down | KeyCode::Char('j')) => {
                        if let Some(ref mut lv) = app.live_view {
                            lv.scroll_down();
                        }
                    }
                    (View::LiveTail, KeyCode::Up | KeyCode::Char('k')) => {
                        if let Some(ref mut lv) = app.live_view {
                            lv.scroll_up();
                        }
                    }
                    (View::LiveTail, KeyCode::Char('G')) => {
                        if let Some(ref mut lv) = app.live_view {
                            lv.scroll_bottom();
                        }
                    }
                    _ => {}
                }
            }
        }

        // Drain channel messages
        while let Ok(msg) = scan_rx.try_recv() {
            match msg {
                ScanMsg::SessionList(mut sessions) => {
                    queue_missing_titles(
                        &mut sessions,
                        &inflight_titles,
                        &active_titles,
                        &title_gate,
                    );
                    app.update_sessions(sessions);
                }
                ScanMsg::Detail(detail) => app.update_detail(detail),
                ScanMsg::StateDebug(info, exp) => {
                    let lines = ui::build_state_debug_content(&info, &exp);
                    app.update_state_debug(info, exp, lines);
                }
                ScanMsg::Usage(u) => {
                    let line = ui::build_usage_line(&u);
                    app.update_usage(u, line);
                }
                ScanMsg::Metrics(m) => {
                    app.update_metrics(m);
                }
                ScanMsg::MetricsProgress { scanned, total } => {
                    app.update_metrics_progress(scanned, total);
                }
                ScanMsg::GhCreateDone { name, result } => {
                    if let Some(picker) = app.folder_picker.as_mut() {
                        picker.reload();
                        if result.is_ok() {
                            if let Some(idx) = picker.entries.iter().position(|e| e == &name) {
                                picker.selection = idx;
                            }
                        }
                    }
                    let status = match result {
                        Ok(url) if !url.is_empty() => format!("created {} — press space to spawn", url),
                        Ok(_) => format!("created {} — press space to spawn", name),
                        Err(e) => format!("gh create failed: {}", e),
                    };
                    app.set_status(status);
                }
            }
        }

        // If a prompt was queued for an auto-spawned session, send it once the
        // session reports Idle in the latest scan.
        match app.poll_pending_dispatch() {
            app::DispatchAction::Send { tmux, prompt } => {
                log::info!(
                    "dispatch: pending target [{}] now idle, sending (len={})",
                    tmux, prompt.len()
                );
                let status = match send::send_prompt(&tmux, &prompt) {
                    Ok(()) => format!("dispatched queued prompt to [{}]", tmux),
                    Err(e) => {
                        log::warn!("dispatch: queued send_prompt failed: {}", e);
                        format!("queued dispatch failed: {}", e)
                    }
                };
                app.set_status(status);
            }
            app::DispatchAction::Timeout { tmux } => {
                log::warn!("dispatch: pending target [{}] never became idle", tmux);
                app.set_status(format!(
                    "queued dispatch timed out — [{}] never became idle",
                    tmux
                ));
            }
            app::DispatchAction::Wait => {}
        }

        if app.should_quit {
            app.log_state_dump();
            break;
        }
    }

    Ok(())
}
