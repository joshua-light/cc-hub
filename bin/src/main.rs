#![allow(clippy::collapsible_match)]

use cc_hub_lib::{
    app, auto_review, clipboard, config, conversation, focus, gh, live_view, metrics, models,
    platform, projects_scan, scanner, send, spawn, title, tmux_pane, triage, ui, usage, watcher,
};

use app::{App, Tab, View};

mod cli;

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
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// How long to suppress re-titling a session/task after a successful run.
/// Long enough to outlast any in-flight `projects_scan::scan` snapshot
/// captured before the title hit disk, so the same id doesn't get titled
/// twice when the snap is drained after the persist.
const TITLE_SUCCESS_COOLDOWN: Duration = Duration::from_secs(30);
/// How long to suppress re-titling after a failed run. Long enough to
/// avoid re-spawning a failing subprocess every scan tick, short enough
/// that a transient fault clears within a few minutes.
const TITLE_FAILURE_COOLDOWN: Duration = Duration::from_secs(300);
/// Initial deadline written when a titler is dispatched. Tightened to one
/// of the two cooldowns above when the spawned task finishes; this just
/// ensures concurrent scans see the id as "in flight" until then.
const TITLE_INFLIGHT_SENTINEL: Duration = Duration::from_secs(3600);

/// Spawn a background `cc-hub-new -p` per session that has a first user
/// message but no cached title yet. `inflight` is a deadline map: each sid
/// is suppressed from re-kickoff until its `Instant` passes, so the
/// titler's persist + the next scan can settle without racing. `active` is
/// the narrower set of sids whose subprocess is actually running right
/// now, and drives the UI spinner.
fn queue_missing_titles(
    sessions: &mut [models::SessionInfo],
    inflight: &Arc<Mutex<HashMap<String, Instant>>>,
    active: &Arc<Mutex<HashSet<String>>>,
    gate: &Arc<tokio::sync::Semaphore>,
) {
    for session in sessions.iter() {
        if session.title.is_some() {
            continue;
        }
        // Skip Inactive sessions — they're synthesized from orphan JSONLs of
        // dead processes, so spending Haiku tokens to title them only pays
        // off cosmetically and re-burns every scan if the title fails.
        if session.state == models::SessionState::Inactive {
            continue;
        }
        let Some(first_msg) = session.summary.clone() else {
            continue;
        };
        let sid = session.session_id.clone();
        {
            let mut lock = inflight.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(&deadline) = lock.get(&sid) {
                if deadline > Instant::now() {
                    continue;
                }
            }
            // Sentinel deadline while in flight; the spawned task tightens
            // this to a success/failure cooldown when it finishes.
            lock.insert(sid.clone(), Instant::now() + TITLE_INFLIGHT_SENTINEL);
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

            let Some(t) = title_result else {
                log::warn!(
                    "title: generation failed for {}, retrying after cooldown",
                    models::short_sid(&sid)
                );
                inflight
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(sid.clone(), Instant::now() + TITLE_FAILURE_COOLDOWN);
                return;
            };

            let sid_for_persist = sid.clone();
            let t_for_persist = t.clone();
            let persist = tokio::task::spawn_blocking(move || {
                title::persist_title(&sid_for_persist, &t_for_persist)
            })
            .await;
            match persist {
                Ok(Ok(())) => log::info!("title: sid={} → {:?}", models::short_sid(&sid), t),
                Ok(Err(e)) => log::warn!("title: persist failed for {}: {}", sid, e),
                Err(e) => log::warn!("title: persist task panicked for {}: {}", sid, e),
            }

            // Success cooldown outlasts any in-flight scan that captured
            // the pre-persist `title: None` snapshot, so the next drain
            // observes the cooldown and skips re-titling.
            inflight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(sid.clone(), Instant::now() + TITLE_SUCCESS_COOLDOWN);
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

/// Mirror of [`queue_missing_titles`] for project tasks: kick off a Haiku
/// titler per task whose `prompt` is set but `title` is still `None`, and
/// stamp `snap.titling` with the in-flight task ids so the UI can render a
/// spinner. Shares the session title concurrency semaphore — both come from
/// the same Haiku subprocess pool, so a second gate would only let twice as
/// many `cc-hub-new -p` children run concurrently for no real win.
fn queue_missing_task_titles(
    snap: &mut projects_scan::ProjectsSnapshot,
    inflight: &Arc<Mutex<HashMap<String, Instant>>>,
    active: &Arc<Mutex<HashSet<String>>>,
    gate: &Arc<tokio::sync::Semaphore>,
) {
    for tasks in snap.tasks.values() {
        for t in tasks {
            if t.title.is_some() {
                continue;
            }
            if t.prompt.trim().is_empty() {
                continue;
            }
            let task_id = t.task_id.clone();
            let project_id = t.project_id.clone();
            let prompt = t.prompt.clone();
            {
                let mut lock = inflight.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(&deadline) = lock.get(&task_id) {
                    if deadline > Instant::now() {
                        continue;
                    }
                }
                lock.insert(task_id.clone(), Instant::now() + TITLE_INFLIGHT_SENTINEL);
            }
            let inflight = Arc::clone(inflight);
            let active = Arc::clone(active);
            let gate = Arc::clone(gate);
            tokio::spawn(async move {
                let _permit = gate.acquire_owned().await.ok();

                active
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(task_id.clone());

                let title_result = tokio::task::spawn_blocking({
                    let p = prompt.clone();
                    move || title::generate_title_blocking(&p)
                })
                .await
                .ok()
                .flatten();

                active
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&task_id);

                let Some(t) = title_result else {
                    log::warn!(
                        "title: task generation failed for {}, retrying after cooldown",
                        task_id
                    );
                    inflight
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(task_id.clone(), Instant::now() + TITLE_FAILURE_COOLDOWN);
                    return;
                };

                let project_id_for_persist = project_id.clone();
                let task_id_for_persist = task_id.clone();
                let title_for_persist = t.clone();
                let persist = tokio::task::spawn_blocking(move || {
                    cc_hub_lib::orchestrator::set_task_title(
                        &project_id_for_persist,
                        &task_id_for_persist,
                        &title_for_persist,
                    )
                })
                .await;
                match persist {
                    Ok(Ok(_)) => log::info!("title: task={} → {:?}", task_id, t),
                    Ok(Err(e)) => {
                        log::warn!("title: persist task title failed for {}: {}", task_id, e)
                    }
                    Err(e) => log::warn!(
                        "title: persist task title task panicked for {}: {}",
                        task_id,
                        e
                    ),
                }

                inflight
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(task_id.clone(), Instant::now() + TITLE_SUCCESS_COOLDOWN);
            });
        }
    }

    let set = active.lock().unwrap_or_else(|e| e.into_inner());
    snap.titling = set.clone();
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
    Projects(projects_scan::ProjectsSnapshot),
    BacklogTriage {
        promotion: Option<triage::Promotion>,
        status: Option<String>,
    },
    AutoReview {
        spawn: Option<auto_review::Spawn>,
        status: Option<String>,
    },
}

/// Spawn the OS-default opener for `path` and detach immediately. URLs work
/// the same as files because `xdg-open` / `open` / `cmd start` all dispatch
/// by scheme. Output is dropped — we don't surface stderr because most
/// failures here mean "no DE installed", which the status bar already
/// reports via the `Err` path of [`std::process::Command::spawn`].
fn open_path_detached(path: &str) -> io::Result<()> {
    use std::process::{Command, Stdio};
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(path);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/c", "start", "", path]);
        c
    };
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(path);
        c
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

/// Size for a popup tmux pane: terminal minus a margin, with floor. The
/// renderer re-resizes on first draw, so a rough starting size is fine.
fn popup_pane_size(terminal: &Terminal<CrosstermBackend<io::Stdout>>) -> (u16, u16) {
    terminal
        .size()
        .map(|s| {
            (
                s.width.saturating_sub(6).max(20),
                s.height.saturating_sub(6).max(10),
            )
        })
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
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = cli::dispatch(&argv) {
        std::process::exit(code);
    }
    if argv.iter().any(|a| a == "--no-tui") {
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
    // Querying the terminal must happen on the alt screen but before we
    // hand stdout to ratatui's backend. On terminals that don't reply (or
    // swallow the probe — e.g. tmux without passthrough), fall back to a
    // sensible 8x16 cell so image cards still render via halfblocks instead
    // of crashing. `from_fontsize` is deprecated upstream in favour of
    // `halfblocks`, but we want the explicit cell size to drive sizing.
    #[allow(deprecated)]
    let image_picker = cc_hub_lib::ratatui_image::picker::Picker::from_query_stdio()
        .unwrap_or_else(|_| cc_hub_lib::ratatui_image::picker::Picker::from_fontsize((8, 16)));
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, image_picker).await;

    // Ask any still-running title subprocesses to kill themselves so the
    // tokio runtime's shutdown doesn't wait up to ~45s on a hung `claude
    // -p`. Blocking tasks can't be cancelled, but they poll this flag.
    title::request_shutdown();
    // Best-effort: flush the log backend so any warn lines emitted just
    // before exit make it to disk even if a panic-while-logging holds the
    // backend's mutex.
    log::logger().flush();

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
            "{:>7}:{} [{:<17}] {:<10} {:<24} {}",
            s.pid,
            models::short_sid(&s.session_id),
            s.state,
            s.agent_badge(),
            s.project_name,
            last_msg
        );
    }
    println!("— {} sessions —", sessions.len());
    Ok(())
}

async fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    image_picker: cc_hub_lib::ratatui_image::picker::Picker,
) -> io::Result<()> {
    let mut app = App::new();
    app.image_picker = Some(image_picker);

    let inflight_titles: Arc<Mutex<HashMap<String, Instant>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let active_titles: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let title_gate: Arc<tokio::sync::Semaphore> =
        Arc::new(tokio::sync::Semaphore::new(config::get().title.concurrency));
    // Task titles share the session title's Haiku subprocess pool — both
    // hit the same `cc-hub-new -p` resource, so doubling the concurrency
    // would buy nothing. Inflight + active sets are scoped per-domain so a
    // session and a task with the same id (impossible in practice, but
    // cheap to keep separate) can't collide.
    let inflight_task_titles: Arc<Mutex<HashMap<String, Instant>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let active_task_titles: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

    let (scan_tx, mut scan_rx) = mpsc::channel::<ScanMsg>(16);
    let (detail_tx, mut detail_rx) = mpsc::channel::<String>(4);
    let (state_debug_tx, mut state_debug_rx) = mpsc::channel::<String>(4);

    let usage_tx = scan_tx.clone();
    let scan_tx_main = scan_tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(config::get().scan.usage_refresh_interval());
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

    // Background backlog triage. Off unless [backlog].enabled — the tick
    // spawns a Claude subprocess and we don't want to surprise users with
    // billed calls.
    if config::get().backlog.enabled {
        let triage_tx = scan_tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(config::get().backlog.interval());
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let outcome = match tokio::task::spawn_blocking(triage::tick).await {
                    Ok(o) => o,
                    Err(e) => {
                        log::warn!("triage: spawn_blocking joined with error: {}", e);
                        continue;
                    }
                };
                if outcome.promotion.is_none() && outcome.status.is_none() {
                    continue;
                }
                let _ = triage_tx
                    .send(ScanMsg::BacklogTriage {
                        promotion: outcome.promotion,
                        status: outcome.status,
                    })
                    .await;
            }
        });
    }

    // Background auto-reviewer. Off unless [auto_review].enabled — every
    // tick may spawn a full reviewer agent session (billed). Mirrors the
    // backlog triage shape: at most one reviewer per tick, eligibility
    // gated by per-task `last_auto_reviewed_at` so each Review round gets
    // exactly one auto-review pass.
    if config::get().auto_review.enabled {
        let review_tx = scan_tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(config::get().auto_review.interval());
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let outcome = match tokio::task::spawn_blocking(auto_review::tick).await {
                    Ok(o) => o,
                    Err(e) => {
                        log::warn!("auto_review: spawn_blocking joined with error: {}", e);
                        continue;
                    }
                };
                if outcome.spawn.is_none() && outcome.status.is_none() {
                    continue;
                }
                let _ = review_tx
                    .send(ScanMsg::AutoReview {
                        spawn: outcome.spawn,
                        status: outcome.status,
                    })
                    .await;
            }
        });
    }

    // Fallback timer catches PID deaths (not a filesystem event) and events
    // missed when a watched dir is rotated or recreated. Its initial tick
    // fires immediately, serving as the startup scan.
    let (fs_tx, mut fs_rx) = mpsc::channel::<()>(8);
    watcher::spawn_fs_watcher(fs_tx);

    tokio::spawn(async move {
        let mut fallback = tokio::time::interval(config::get().scan.fs_fallback_interval());
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
                    let snap = tokio::task::spawn_blocking(projects_scan::scan)
                        .await
                        .unwrap_or_else(|_| projects_scan::ProjectsSnapshot::empty());
                    let _ = scan_tx.send(ScanMsg::Projects(snap)).await;
                }
                Some(()) = fs_rx.recv() => {
                    // Drain coalesced signals — one scan per burst is enough.
                    while fs_rx.try_recv().is_ok() {}
                    let sessions = tokio::task::spawn_blocking(scanner::scan_sessions)
                        .await
                        .unwrap_or_default();
                    latest_sessions = sessions.clone();
                    let _ = scan_tx.send(ScanMsg::SessionList(sessions)).await;
                    let snap = tokio::task::spawn_blocking(projects_scan::scan)
                        .await
                        .unwrap_or_else(|_| projects_scan::ProjectsSnapshot::empty());
                    let _ = scan_tx.send(ScanMsg::Projects(snap)).await;
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
                        let _ = progress_tx.try_send(ScanMsg::MetricsProgress { scanned, total });
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

        if app.view == View::TmuxPane && app.tmux_pane.as_ref().is_some_and(|p| p.is_exited()) {
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
                let on_projects = app.view == View::Grid && app.current_tab == Tab::Projects;

                match (&app.view, key.code) {
                    // Quit
                    (View::Grid, KeyCode::Char('q')) => {
                        app.should_quit = true;
                    }
                    (View::Grid, KeyCode::Tab | KeyCode::BackTab) => {
                        let was_metrics = app.current_tab == Tab::Metrics;
                        app.cycle_tab();
                        if !was_metrics && app.current_tab == Tab::Metrics && app.metrics.is_none()
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
                    (View::Grid, KeyCode::Up | KeyCode::Char('k')) if on_sessions => app.move_up(),
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
                    // Kanban: j/k moves the row cursor within the focused
                    // column; h/l switches column; H/L (or [/]) cycles project chips.
                    (View::Grid, KeyCode::Down | KeyCode::Char('j')) if on_projects => {
                        app.projects_task_next();
                    }
                    (View::Grid, KeyCode::Up | KeyCode::Char('k')) if on_projects => {
                        app.projects_task_prev();
                    }
                    (View::Grid, KeyCode::Right | KeyCode::Char('l')) if on_projects => {
                        app.projects_col_right();
                    }
                    (View::Grid, KeyCode::Left | KeyCode::Char('h')) if on_projects => {
                        app.projects_col_left();
                    }
                    (View::Grid, KeyCode::Char(']') | KeyCode::Char('L')) if on_projects => {
                        app.projects_move_down();
                    }
                    (View::Grid, KeyCode::Char('[') | KeyCode::Char('H')) if on_projects => {
                        app.projects_move_up();
                    }
                    (View::Grid, KeyCode::Char(' ')) if on_projects => {
                        use cc_hub_lib::app::ApproveOutcome;
                        let target = app.selected_project_task().cloned();
                        match app.approve_review_task() {
                            ApproveOutcome::NotReviewTask => {
                                app.set_status("nothing to approve (focus a Review task)".into());
                            }
                            ApproveOutcome::DoneNoPr | ApproveOutcome::Failed => {}
                            ApproveOutcome::PrApproved => {
                                let Some(task) = target else { continue };
                                let short = cc_hub_lib::orchestrator::short_task_id(&task.task_id);
                                let Some(tmux_name) = task.orchestrator_tmux.clone() else {
                                    app.set_status(format!(
                                        "approved {} but task has no orchestrator tmux to notify",
                                        short
                                    ));
                                    continue;
                                };
                                if !send::tmux_session_exists(&tmux_name) {
                                    app.set_status(format!(
                                        "approved {} but orchestrator [{}] is not live",
                                        short, tmux_name
                                    ));
                                    continue;
                                }
                                let prompt = match std::env::current_exe() {
                                    Ok(bin) => {
                                        cc_hub_lib::orchestrator::build_review_approval_prompt(
                                            &task.task_id,
                                            &bin,
                                        )
                                    }
                                    Err(e) => {
                                        log::warn!(
                                            "approve: current_exe failed while building notify prompt: {}",
                                            e
                                        );
                                        cc_hub_lib::orchestrator::build_review_approval_prompt(
                                            &task.task_id,
                                            std::path::Path::new("cc-hub"),
                                        )
                                    }
                                };
                                if send::pane_ready_for_input(&tmux_name) {
                                    let status = match send::send_prompt(&tmux_name, &prompt) {
                                        Ok(()) => format!(
                                            "approved {} and notified orchestrator [{}] to continue merge flow",
                                            short, tmux_name
                                        ),
                                        Err(e) => {
                                            log::warn!("approve: send_prompt failed: {}", e);
                                            format!(
                                                "approved {} but orchestrator notify failed: {}",
                                                short, e
                                            )
                                        }
                                    };
                                    app.set_status(status);
                                } else {
                                    app.queue_pending_dispatch(tmux_name.clone(), prompt);
                                    app.set_status(format!(
                                        "approved {} — queued notify for orchestrator [{}] when idle",
                                        short, tmux_name
                                    ));
                                }
                            }
                        }
                    }
                    (View::Grid, KeyCode::Char('r')) if on_projects => {
                        if !app.enter_projects_result() {
                            app.set_status("no task selected".into());
                        }
                    }
                    (
                        View::ProjectsResult,
                        KeyCode::Esc | KeyCode::Char('r') | KeyCode::Char('q'),
                    ) => {
                        app.close_projects_result();
                    }
                    (View::ProjectsResult, KeyCode::Down | KeyCode::Char('j')) => {
                        app.result_artifact_next();
                    }
                    (View::ProjectsResult, KeyCode::Up | KeyCode::Char('k')) => {
                        app.result_artifact_prev();
                    }
                    (View::ProjectsResult, KeyCode::PageDown) => {
                        app.result_scroll_by(10);
                    }
                    (View::ProjectsResult, KeyCode::PageUp) => {
                        app.result_scroll_by(-10);
                    }
                    (View::ProjectsResult, KeyCode::Char('c')) => {
                        match app.selected_result_artifact().map(|a| a.path.clone()) {
                            None => app.set_status("no artifact to copy".into()),
                            Some(path) => match clipboard::copy(&path) {
                                Ok(()) => app.set_status(format!("copied: {}", path)),
                                Err(e) => app.set_status(format!("copy failed: {}", e)),
                            },
                        }
                    }
                    (View::ProjectsResult, KeyCode::Char('e')) => {
                        app.toggle_result_artifact_expanded();
                    }
                    (View::ProjectsResult, KeyCode::Char('o')) => {
                        match app.selected_result_artifact().map(|a| a.path.clone()) {
                            None => app.set_status("no artifact to open".into()),
                            Some(path) => {
                                let result = open_path_detached(&path);
                                match result {
                                    Ok(()) => app.set_status(format!("opening {}", path)),
                                    Err(e) => app.set_status(format!("open failed: {}", e)),
                                }
                            }
                        }
                    }
                    (View::Grid, KeyCode::Char('c')) if on_projects => {
                        match app.selected_project_task().map(|t| t.task_id.clone()) {
                            None => app.set_status("no task selected".into()),
                            Some(task_id) => match clipboard::copy(&task_id) {
                                Ok(()) => app.set_status(format!("copied task id: {}", task_id)),
                                Err(e) => app.set_status(format!("copy failed: {}", e)),
                            },
                        }
                    }
                    (View::Grid, KeyCode::Char('N')) if on_projects => {
                        // Register a project (folder picker), no task spawn.
                        // Use `n` to start a task on an existing project.
                        app.enter_folder_picker_for_register_only();
                    }
                    (View::Grid, KeyCode::Char('n')) if on_projects => {
                        // Start a new task on the currently-selected project.
                        if !app.enter_project_task_prompt_for_selected() {
                            app.set_status("no project selected — press N to register one".into());
                        }
                    }
                    (View::Grid, KeyCode::Enter) if on_projects => {
                        // Open the orchestrator's tmux session embedded in the
                        // TUI — same mechanism the Sessions view uses for `f`
                        // / Enter on a live session.
                        if let Some(task) = app.selected_project_task().cloned() {
                            match task.orchestrator_tmux.as_deref() {
                                None => {
                                    app.set_status(
                                        "task has no orchestrator tmux session yet".into(),
                                    );
                                }
                                Some(tmux_name) => {
                                    let (cols, rows) = popup_pane_size(terminal);
                                    match tmux_pane::TmuxPaneView::spawn(tmux_name, rows, cols) {
                                        Ok(pane) => app.enter_tmux_pane(pane),
                                        Err(e) => app
                                            .set_status(format!("open orchestrator failed: {}", e)),
                                    }
                                }
                            }
                        } else {
                            app.set_status(
                                "no task selected — focus a task on the kanban first".into(),
                            );
                        }
                    }
                    (View::Grid, KeyCode::Char('f')) if on_projects => {
                        if let Some(task) = app.selected_project_task().cloned() {
                            let live_tmux = task
                                .orchestrator_tmux
                                .as_deref()
                                .filter(|n| send::tmux_session_exists(n));
                            let resurrectable = if live_tmux.is_none()
                                && matches!(
                                    task.status,
                                    cc_hub_lib::orchestrator::TaskStatus::Running
                                        | cc_hub_lib::orchestrator::TaskStatus::Review
                                ) {
                                scanner::find_orchestrator_session(
                                    &task.project_root,
                                    &task.task_id,
                                    task.orchestrator_agent_kind,
                                    task.orchestrator_session_id.as_deref(),
                                )
                            } else {
                                None
                            };
                            if let Some(tmux_name) = live_tmux {
                                let (cols, rows) = popup_pane_size(terminal);
                                match tmux_pane::TmuxPaneView::spawn(tmux_name, rows, cols) {
                                    Ok(pane) => app.enter_tmux_pane(pane),
                                    Err(e) => {
                                        app.set_status(format!("open orchestrator failed: {}", e))
                                    }
                                }
                            } else if let Some(resume) = resurrectable {
                                let cwd = task.project_root.to_string_lossy().into_owned();
                                match spawn::spawn_agent_session(
                                    &task.orchestrator_agent_id,
                                    &cwd,
                                    Some(resume.resume.clone()),
                                    None,
                                    false,
                                ) {
                                    Ok(new_tmux) => {
                                        if let Err(e) = cc_hub_lib::orchestrator::update_task_state(
                                            &task.project_id,
                                            &task.task_id,
                                            |s| {
                                                s.orchestrator_tmux = Some(new_tmux.clone());
                                                s.orchestrator_session_id =
                                                    Some(resume.session_id.clone());
                                            },
                                        ) {
                                            app.set_status(format!(
                                                "resurrected [{}] but state write failed: {}",
                                                new_tmux, e
                                            ));
                                        }
                                        let (cols, rows) = popup_pane_size(terminal);
                                        match tmux_pane::TmuxPaneView::spawn(&new_tmux, rows, cols)
                                        {
                                            Ok(pane) => {
                                                app.set_status(format!(
                                                    "resumed orchestrator {} [{}]",
                                                    models::short_sid(&resume.session_id),
                                                    new_tmux
                                                ));
                                                app.enter_tmux_pane(pane);
                                            }
                                            Err(e) => app.set_status(format!(
                                                "resurrected [{}] but attach failed: {}",
                                                new_tmux, e
                                            )),
                                        }
                                    }
                                    Err(e) => app.set_status(format!("resurrect failed: {}", e)),
                                }
                            } else if let Some(log_path) =
                                cc_hub_lib::orchestrator::task_orchestrator_log_path(
                                    &task.project_id,
                                    &task.task_id,
                                )
                                .filter(|p| p.exists())
                            {
                                let (cols, rows) = popup_pane_size(terminal);
                                match spawn::spawn_log_viewer_tmux_session(&log_path) {
                                    Ok(name) => match tmux_pane::TmuxPaneView::spawn_owned(
                                        &name, rows, cols,
                                    ) {
                                        Ok(pane) => app.enter_tmux_pane(pane),
                                        Err(e) => app
                                            .set_status(format!("log viewer attach failed: {}", e)),
                                    },
                                    Err(e) => {
                                        app.set_status(format!("log viewer spawn failed: {}", e))
                                    }
                                }
                            } else if matches!(
                                task.status,
                                cc_hub_lib::orchestrator::TaskStatus::Running
                                    | cc_hub_lib::orchestrator::TaskStatus::Review
                            ) {
                                let session_store = match task.orchestrator_agent_kind {
                                    cc_hub_lib::agent::AgentKind::Claude => "~/.claude/projects/",
                                    cc_hub_lib::agent::AgentKind::Pi => "~/.pi/agent/sessions/",
                                };
                                let detail = match task.orchestrator_session_id.as_deref() {
                                    Some(sid) => format!(
                                        "orchestrator dead — sid {} not found under {} (cwd {}); no JSONL contains orchestrator prompt for task {}",
                                        models::short_sid(sid),
                                        session_store,
                                        task.project_root.display(),
                                        &task.task_id,
                                    ),
                                    None => format!(
                                        "orchestrator dead — no JSONL under {} contains orchestrator prompt for task {} (cwd {})",
                                        session_store,
                                        &task.task_id,
                                        task.project_root.display(),
                                    ),
                                };
                                app.set_status(detail);
                            } else {
                                app.set_status("no orchestrator log available".into());
                            }
                        } else {
                            app.set_status(
                                "no task selected — focus a task on the kanban first".into(),
                            );
                        }
                    }
                    (View::Grid, KeyCode::Char('R')) if on_projects => {
                        app.enter_confirm_task_restart();
                    }
                    (View::Grid, KeyCode::Char('x')) if on_projects => {
                        app.enter_confirm_task_delete();
                    }
                    (View::Grid, KeyCode::Char('X')) if on_projects => {
                        app.enter_confirm_project_delete();
                    }
                    (View::Grid, KeyCode::Char('b')) if on_projects => {
                        app.open_backlog();
                    }
                    (View::Backlog, KeyCode::Esc | KeyCode::Char('q')) => {
                        app.close_backlog();
                    }
                    (View::Backlog, KeyCode::Down | KeyCode::Char('j')) => {
                        app.backlog_down();
                    }
                    (View::Backlog, KeyCode::Up | KeyCode::Char('k')) => {
                        app.backlog_up();
                    }
                    (View::Backlog, KeyCode::Char('s') | KeyCode::Enter)
                    | (View::Grid, KeyCode::Char('s'))
                        if on_projects || matches!(app.view, View::Backlog) =>
                    {
                        let Some(p) = app.selected_project().cloned() else {
                            app.set_status("no project selected".into());
                            continue;
                        };
                        let task_opt = if matches!(app.view, View::Backlog) {
                            app.selected_backlog_task().cloned()
                        } else {
                            app.selected_project_task().cloned()
                        };
                        let Some(task) = task_opt else {
                            app.set_status("no task selected".into());
                            continue;
                        };
                        if task.status != cc_hub_lib::orchestrator::TaskStatus::Backlog {
                            app.set_status(format!(
                                "task is not in backlog (status = {:?})",
                                task.status
                            ));
                            continue;
                        }
                        match cc_hub_lib::orchestrator::start_backlog_task(
                            &p.id,
                            &task.task_id,
                            None,
                        ) {
                            Ok((state, tmux_name, orch_prompt)) => {
                                if let Some(prompt) = orch_prompt {
                                    app.queue_pending_dispatch(tmux_name.clone(), prompt);
                                }
                                log::info!(
                                    "project task: started backlog {} orchestrator [{}]",
                                    state.task_id,
                                    tmux_name
                                );
                                app.set_status(format!(
                                    "task started [{}], orchestrator [{}] starting…",
                                    state.task_id, tmux_name
                                ));
                                if matches!(app.view, View::Backlog) {
                                    app.close_backlog();
                                }
                                app.pending_focus_task_id = Some(state.task_id.clone());
                                app.pending_focus_budget = 5;
                            }
                            Err(e) => {
                                log::warn!("project task: start backlog failed: {}", e);
                                app.set_status(format!("start backlog failed: {}", e));
                            }
                        }
                    }
                    (View::Grid, KeyCode::Enter) if on_metrics => {
                        if let Some(row) = app.selected_metrics_session().cloned() {
                            let agent_kind = if platform::paths::pi_sessions_dir()
                                .as_ref()
                                .is_some_and(|dir| row.jsonl_path.starts_with(dir))
                            {
                                cc_hub_lib::agent::AgentKind::Pi
                            } else {
                                cc_hub_lib::agent::AgentKind::Claude
                            };
                            let lv = live_view::LiveView::review(
                                row.jsonl_path.clone(),
                                agent_kind,
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
                    (View::Grid, KeyCode::Char('W')) if on_sessions => {
                        app.toggle_show_orch_workers();
                        let state = if app.show_orch_workers {
                            "shown"
                        } else {
                            "hidden"
                        };
                        app.set_status(format!("orchestrator/worker sessions {}", state));
                    }
                    (View::Grid, KeyCode::Char('f') | KeyCode::Enter) if on_sessions => {
                        if let Some(session) = app.selected_session_info().cloned() {
                            if session.state == models::SessionState::Inactive {
                                let resume = match session.agent_kind {
                                    cc_hub_lib::agent::AgentKind::Claude => Some(
                                        spawn::ResumeTarget::SessionId(session.session_id.clone()),
                                    ),
                                    cc_hub_lib::agent::AgentKind::Pi => session
                                        .jsonl_path
                                        .clone()
                                        .map(spawn::ResumeTarget::SessionFile),
                                };
                                let status = match resume {
                                    Some(target) => match spawn::spawn_agent_session(
                                        &session.agent_id,
                                        &session.cwd,
                                        Some(target),
                                        None,
                                        false,
                                    ) {
                                        Ok(name) => format!(
                                            "resumed {} [{}]",
                                            models::short_sid(&session.session_id),
                                            name
                                        ),
                                        Err(e) => format!("resume failed: {}", e),
                                    },
                                    None => "resume failed: missing session transcript".to_string(),
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
                                        let msg =
                                            match spawn::attach_tmux_session(&name, &session.cwd) {
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
                                Ok(tmux_name) => match tmux_pane::TmuxPaneView::spawn_owned(
                                    &tmux_name, rows, cols,
                                ) {
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
                        if let Some(pending) = app.take_pending_project_delete() {
                            let msg =
                                match cc_hub_lib::orchestrator::remove_project(&pending.project_id)
                                {
                                    Ok(()) => format!("removed {}", pending.display),
                                    Err(e) => format!("remove failed: {}", e),
                                };
                            // Selection may dangle past the now-removed project
                            // until the next scan tick lands; reset to 0 so
                            // we don't render one bad frame.
                            app.projects_sel = 0;
                            app.set_status(msg);
                        } else if let Some(pending) = app.take_pending_task_delete() {
                            // Best-effort kill of the orchestrator tmux —
                            // workers stay alive (their parent task is
                            // gone, but the running claude is independent).
                            let kill_result = pending
                                .orchestrator_tmux
                                .as_deref()
                                .map(send::kill_tmux_session);
                            // Remove the on-disk state so the Projects view
                            // refreshes the entry away on next scan.
                            let task_dir = cc_hub_lib::orchestrator::task_state_dir(
                                &pending.project_id,
                                &pending.task_id,
                            );
                            let removal = task_dir.as_ref().map(std::fs::remove_dir_all);
                            let kill_msg = match kill_result {
                                Some(Ok(())) => "orchestrator killed",
                                Some(Err(_)) => "orchestrator kill failed",
                                None => "no orchestrator to kill",
                            };
                            let removal_msg = match removal {
                                Some(Ok(())) => "state removed",
                                Some(Err(e)) => {
                                    log::warn!("task delete: rm state.json: {}", e);
                                    "state removal failed"
                                }
                                None => "no state path",
                            };
                            app.set_status(format!(
                                "deleted {} ({}, {})",
                                pending.display, kill_msg, removal_msg
                            ));
                        } else if let Some(pending) = app.take_pending_task_restart() {
                            match cc_hub_lib::orchestrator::restart_task(
                                &pending.project_id,
                                &pending.task_id,
                                None,
                            ) {
                                Ok((state, tmux_name, orch_prompt)) => {
                                    if let Some(prompt) = orch_prompt {
                                        app.queue_pending_dispatch(tmux_name.clone(), prompt);
                                    }
                                    log::info!(
                                        "project task: restarted {} orchestrator [{}]",
                                        state.task_id,
                                        tmux_name
                                    );
                                    app.set_status(format!(
                                        "restarted [{}], orchestrator [{}] starting…",
                                        state.task_id, tmux_name
                                    ));
                                    app.pending_focus_task_id = Some(state.task_id.clone());
                                    app.pending_focus_budget = 5;
                                }
                                Err(e) => {
                                    log::warn!("project task: restart failed: {}", e);
                                    app.set_status(format!("restart failed: {}", e));
                                }
                            }
                        } else if let Some(pending) = app.take_pending_close() {
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
                            let status = match spawn::spawn_agent_session(
                                &sess.agent_id,
                                &sess.cwd,
                                None,
                                None,
                                false,
                            ) {
                                Ok(name) => format!("started {} [{}]", sess.agent_badge(), name),
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
                    (
                        View::FolderPicker,
                        KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h'),
                    ) => {
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
                        let projects_mode = app.creating_project_task;
                        let register_mode = app.registering_project_only;
                        if let Some(cwd) = cwd {
                            if register_mode {
                                let status = match app.register_picked_project(&cwd) {
                                    Ok(name) => format!("registered project: {}", name),
                                    Err(e) => format!("register failed: {}", e),
                                };
                                app.set_status(status);
                            } else if projects_mode {
                                app.enter_project_task_prompt(cwd);
                            } else {
                                app.close_folder_picker();
                                let agent_id = config::get().default_session_agent_id();
                                let status = match spawn::spawn_agent_session(
                                    &agent_id, &cwd, None, None, false,
                                ) {
                                    Ok(name) => format!("started {} [{}]", agent_id, name),
                                    Err(e) => format!("spawn failed: {}", e),
                                };
                                app.set_status(status);
                            }
                        } else {
                            app.close_folder_picker();
                        }
                    }
                    (View::FolderPicker, KeyCode::Char('.')) => {
                        let cwd = app
                            .folder_picker
                            .as_ref()
                            .map(|p| p.current_dir.display().to_string());
                        let projects_mode = app.creating_project_task;
                        let register_mode = app.registering_project_only;
                        if let Some(cwd) = cwd {
                            if register_mode {
                                let status = match app.register_picked_project(&cwd) {
                                    Ok(name) => format!("registered project: {}", name),
                                    Err(e) => format!("register failed: {}", e),
                                };
                                app.set_status(status);
                            } else if projects_mode {
                                app.enter_project_task_prompt(cwd);
                            } else {
                                app.close_folder_picker();
                                let agent_id = config::get().default_session_agent_id();
                                let status = match spawn::spawn_agent_session(
                                    &agent_id, &cwd, None, None, false,
                                ) {
                                    Ok(name) => format!("started {} [{}]", agent_id, name),
                                    Err(e) => format!("spawn failed: {}", e),
                                };
                                app.set_status(status);
                            }
                        } else {
                            app.close_folder_picker();
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
                    (View::PromptInput, KeyCode::Tab) => {
                        app.cycle_pending_agent_id();
                    }
                    (View::PromptInput, KeyCode::Backspace) => {
                        app.prompt_buffer.pop();
                    }
                    (View::PromptInput, KeyCode::Char(c)) => {
                        app.prompt_buffer.push(c);
                    }
                    (View::PromptInput, KeyCode::Enter) => {
                        if app.prompt_buffer.trim().is_empty() {
                            app.close_prompt_input();
                            app.set_status("empty prompt — dispatch cancelled".into());
                            continue;
                        }

                        // Projects-tab flow: create task, spawn orchestrator,
                        // queue the orchestrator prompt for dispatch when Idle.
                        if app.prompt_input_for_project() {
                            let Some((cwd, prompt, agent_id)) = app.submit_project_task() else {
                                app.set_status("project task: missing cwd".into());
                                continue;
                            };
                            let project_root = std::path::Path::new(&cwd);
                            let project_name = project_root
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| cwd.clone());
                            match cc_hub_lib::orchestrator::spawn_orchestrator_for_new_task(
                                project_root,
                                &project_name,
                                prompt,
                                agent_id.as_deref(),
                            ) {
                                Ok((state, tmux_name, orch_prompt)) => {
                                    if let Some(prompt) = orch_prompt {
                                        app.queue_pending_dispatch(tmux_name.clone(), prompt);
                                    }
                                    log::info!(
                                        "project task: created {} in {}, orchestrator [{}]",
                                        state.task_id,
                                        cwd,
                                        tmux_name
                                    );
                                    app.set_status(format!(
                                        "task created [{}], orchestrator [{}] starting…",
                                        state.task_id, tmux_name
                                    ));
                                }
                                Err(e) => {
                                    log::warn!("project task: spawn failed: {}", e);
                                    app.set_status(format!("project task failed: {}", e));
                                }
                            }
                            continue;
                        }

                        let target = app.dispatch_target().cloned();
                        let prompt = app.submit_prompt_input();

                        if let Some((pid, name, tmux)) = target {
                            log::info!(
                                "dispatch: idle target {} (PID {}) [{}] prompt_len={}",
                                name,
                                pid,
                                tmux,
                                prompt.len()
                            );
                            let status = match send::send_prompt(&tmux, &prompt) {
                                Ok(()) => {
                                    format!("dispatched to {} (PID {}) [{}]", name, pid, tmux)
                                }
                                Err(e) => {
                                    log::warn!("dispatch: send_prompt failed: {}", e);
                                    format!("dispatch failed: {}", e)
                                }
                            };
                            app.set_status(status);
                            continue;
                        }

                        let Some(cwd) = app.default_spawn_cwd() else {
                            app.set_status("no idle agent and no cwd to spawn in".into());
                            continue;
                        };
                        let agent_id = config::get().default_session_agent_id();
                        let agent = config::get().agent(&agent_id);
                        let supports_initial_prompt =
                            agent.as_ref().is_some_and(|a| a.supports_initial_prompt());
                        match spawn::spawn_agent_session(
                            &agent_id,
                            &cwd,
                            None,
                            if supports_initial_prompt {
                                Some(prompt.as_str())
                            } else {
                                None
                            },
                            false,
                        ) {
                            Ok(tmux_name) => {
                                if supports_initial_prompt {
                                    log::info!(
                                        "dispatch: no idle agent, spawned [{}] in {} with inline prompt (len={})",
                                        tmux_name, cwd, prompt.len()
                                    );
                                    app.set_status(format!(
                                        "no idle agent — spawned {} [{}]",
                                        agent_id, tmux_name
                                    ));
                                } else {
                                    log::info!(
                                        "dispatch: no idle agent, spawned [{}] in {} — queueing prompt (len={})",
                                        tmux_name, cwd, prompt.len()
                                    );
                                    app.queue_pending_dispatch(tmux_name.clone(), prompt);
                                    app.set_status(format!(
                                        "no idle agent — spawned {} [{}], prompt queued",
                                        agent_id, tmux_name
                                    ));
                                }
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
                ScanMsg::Projects(mut snap) => {
                    queue_missing_task_titles(
                        &mut snap,
                        &inflight_task_titles,
                        &active_task_titles,
                        &title_gate,
                    );
                    app.update_projects(snap);
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
                        Ok(url) if !url.is_empty() => {
                            format!("created {} — press space to spawn", url)
                        }
                        Ok(_) => format!("created {} — press space to spawn", name),
                        Err(e) => format!("gh create failed: {}", e),
                    };
                    app.set_status(status);
                }
                ScanMsg::BacklogTriage { promotion, status } => {
                    if let Some(p) = promotion {
                        if let Some(prompt) = p.orchestrator_prompt {
                            app.queue_pending_dispatch(p.tmux, prompt);
                        }
                    }
                    if let Some(s) = status {
                        app.set_status(s);
                    }
                }
                ScanMsg::AutoReview { spawn, status } => {
                    // Claude ignores spawn-time initial prompts, so the
                    // briefing is delivered via tmux send-keys after the
                    // session reaches Idle — same pattern the backlog
                    // triager uses for orchestrator prompts.
                    if let Some(s) = spawn {
                        if let Some(prompt) = s.prompt_to_dispatch {
                            app.queue_pending_dispatch(s.tmux, prompt);
                        }
                    }
                    if let Some(s) = status {
                        app.set_status(s);
                    }
                }
            }
        }

        // If a prompt was queued for an auto-spawned session, send it once the
        // session reports Idle in the latest scan.
        match app.poll_pending_dispatch() {
            app::DispatchAction::Send { tmux, prompt } => {
                log::info!(
                    "dispatch: pending target [{}] now idle, sending (len={})",
                    tmux,
                    prompt.len()
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
