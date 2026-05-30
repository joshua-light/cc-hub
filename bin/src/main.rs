#![allow(clippy::collapsible_match)]

use cc_hub_lib::{
    app, auto_review, config, conversation, metrics, models, platform, projects_scan, scanner,
    send, session_count, spawn, title, triage, ui, usage, watcher,
};

use app::{App, Tab, View};

mod cli;
mod keys;

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
    Event, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
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

pub(crate) enum ScanMsg {
    SessionList(Vec<models::SessionInfo>),
    Detail(models::SessionDetail),
    StateDebug(models::SessionInfo, conversation::StateExplanation),
    Usage(usage::UsageInfo),
    SessionCounts(session_count::SessionCounts),
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
    /// Result of a `send::send_prompt` run off the event-loop thread (see
    /// [`spawn_dispatch`]). `send_prompt` forks+execs tmux twice and sleeps
    /// ~80ms; running it inline froze render+input. `ok` carries the status
    /// line built from the prompt's success/failure templates.
    DispatchResult {
        ok: Result<String, String>,
    },
}

/// Spawn the OS-default opener for `path` and detach immediately. URLs work
/// the same as files because `xdg-open` / `open` / `cmd start` all dispatch
/// by scheme. Output is dropped — we don't surface stderr because most
/// failures here mean "no DE installed", which the status bar already
/// reports via the `Err` path of [`std::process::Command::spawn`].
pub(crate) fn open_path_detached(path: &str) -> io::Result<()> {
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

/// Spawn a session / project task at `cwd` from the folder picker,
/// routing through the right App method based on the picker mode flags.
/// Closes the picker on completion (the App helpers handle that for the
/// register/projects branches).
pub(crate) fn dispatch_picked_cwd(app: &mut App, cwd: &str) {
    if app.registering_project_only {
        let status = match app.register_picked_project(cwd) {
            Ok(name) => format!("registered project: {}", name),
            Err(e) => format!("register failed: {}", e),
        };
        app.set_status(status);
    } else if app.creating_project_task {
        app.enter_project_task_prompt(cwd.to_string());
    } else {
        app.close_folder_picker();
        let agent_id = config::get().default_session_agent_id();
        let status = match spawn::spawn_agent_session(&agent_id, cwd, None, None, false) {
            Ok(name) => format!("started {} [{}]", agent_id, name),
            Err(e) => format!("spawn failed: {}", e),
        };
        app.set_status(status);
    }
}

/// Run `send::send_prompt` off the synchronous run() loop thread and report
/// the outcome back over `tx` as a [`ScanMsg::DispatchResult`], drained in the
/// same channel loop as every other scan message. `send_prompt` forks+execs
/// tmux twice and sleeps ~80ms; called inline it froze render+input for
/// 100-160ms during dispatch. On success the status line is `ok_msg`; on
/// failure it is `"<err_prefix>: <error>"` (and the error is logged), matching
/// the inline templates these call sites used before.
pub(crate) fn spawn_dispatch(
    tx: mpsc::Sender<ScanMsg>,
    tmux: String,
    prompt: String,
    ok_msg: String,
    err_prefix: String,
) {
    tokio::spawn(async move {
        let ok = tokio::task::spawn_blocking(move || send::send_prompt(&tmux, &prompt))
            .await
            .unwrap_or_else(|e| Err(io::Error::other(format!("dispatch task panicked: {}", e))))
            .map(|()| ok_msg)
            .map_err(|e| {
                log::warn!("dispatch: send_prompt failed: {}", e);
                format!("{}: {}", err_prefix, e)
            });
        let _ = tx.send(ScanMsg::DispatchResult { ok }).await;
    });
}

/// Resolve the highlighted picker entry and dispatch it via
/// [`dispatch_picked_cwd`]. Works in both Browse and Bookmarks mode since
/// the lookup goes through `FolderPicker::selected_path`. No-ops by
/// closing the picker when nothing is selected.
pub(crate) fn pick_from_folder_picker(app: &mut App) {
    let cwd = app
        .folder_picker
        .as_ref()
        .and_then(|p| p.selected_path())
        .map(|p| p.display().to_string());
    match cwd {
        Some(cwd) => dispatch_picked_cwd(app, &cwd),
        None => app.close_folder_picker(),
    }
}

/// Size for a popup tmux pane: terminal minus a margin, with floor. The
/// renderer re-resizes on first draw, so a rough starting size is fine.
pub(crate) fn popup_pane_size(terminal: &Terminal<CrosstermBackend<io::Stdout>>) -> (u16, u16) {
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

/// Apply one drained [`ScanMsg`] to `app`. Extracted verbatim from the
/// `run()` channel-drain loop; the `title`-tracking maps/gate are threaded
/// through so the `SessionList` / `Projects` arms can kick off missing-title
/// subprocesses exactly as they did inline.
fn apply_scan_msg(
    app: &mut App,
    msg: ScanMsg,
    inflight_titles: &Arc<Mutex<HashMap<String, Instant>>>,
    active_titles: &Arc<Mutex<HashSet<String>>>,
    inflight_task_titles: &Arc<Mutex<HashMap<String, Instant>>>,
    active_task_titles: &Arc<Mutex<HashSet<String>>>,
    title_gate: &Arc<tokio::sync::Semaphore>,
) {
    match msg {
        ScanMsg::SessionList(mut sessions) => {
            queue_missing_titles(&mut sessions, inflight_titles, active_titles, title_gate);
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
        ScanMsg::SessionCounts(c) => {
            app.update_session_counts(c);
        }
        ScanMsg::Metrics(m) => {
            app.update_metrics(m);
        }
        ScanMsg::MetricsProgress { scanned, total } => {
            app.update_metrics_progress(scanned, total);
        }
        ScanMsg::Projects(mut snap) => {
            queue_missing_task_titles(&mut snap, inflight_task_titles, active_task_titles, title_gate);
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
        ScanMsg::DispatchResult { ok } => {
            // Success and failure both already hold the rendered
            // status line; either way it goes straight to the bar.
            app.set_status(ok.unwrap_or_else(|e| e));
        }
    }
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
            let (usage_opt, counts) = tokio::task::spawn_blocking(|| {
                (usage::fetch_usage(), session_count::count_recent_sessions())
            })
            .await
            .unwrap_or((None, session_count::SessionCounts::default()));
            let _ = usage_tx.send(ScanMsg::SessionCounts(counts)).await;
            if let Some(u) = usage_opt {
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

    // Redraw gating (issue #18a). The loop wakes every ~50ms but the widget
    // tree only changes on input, on a drained ScanMsg, on a LiveTail poll
    // that picked up new entries, or on the once-per-second elapsed clock.
    // We draw immediately on the first three (so input latency is unchanged)
    // and throttle the purely time-driven "refreshed Ns ago" / elapsed-card
    // repaint to ~1Hz. `dirty` starts true so the first frame always renders.
    // The embedded tmux pane is the exception: its content is fed by a
    // background reader, so while it's open we redraw every loop tick.
    let mut dirty = true;
    let mut last_clock_redraw = Instant::now();

    loop {
        // Poll live view for new JSONL entries
        if app.view == View::LiveTail {
            if let Some(ref mut lv) = app.live_view {
                if lv.poll() {
                    dirty = true;
                }
            }
        }

        if app.view == View::TmuxPane && app.tmux_pane.as_ref().is_some_and(|p| p.is_exited()) {
            app.close_tmux_pane();
            dirty = true;
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
            dirty = true;
        }

        // The embedded tmux pane streams content from a background reader, so
        // its widget tree changes without any event we observe here — always
        // repaint while it's open. Otherwise paint only when something
        // changed, plus a ~1Hz tick so the elapsed clocks keep moving.
        let in_tmux = app.view == View::TmuxPane;
        let clock_tick = last_clock_redraw.elapsed() >= Duration::from_secs(1);
        if dirty || in_tmux || clock_tick {
            terminal.draw(|frame| hot::render(frame, &mut app))?;
            dirty = false;
            last_clock_redraw = Instant::now();
        }

        let poll_ms = if app.view == View::TmuxPane { 16 } else { 50 };

        if event::poll(Duration::from_millis(poll_ms))? {
            let evt = event::read()?;
            // Any input may mutate state (scroll, selection, view change,
            // status line) — repaint on the next loop pass regardless of which
            // arm handles it or whether it `continue`s out early.
            dirty = true;
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

                let outcome = keys::handle_key(
                    &mut app,
                    key,
                    terminal,
                    &scan_tx_main,
                    &detail_tx,
                    &state_debug_tx,
                    &spawn_metrics,
                    on_sessions,
                    on_metrics,
                    on_projects,
                )
                .await;
                if let keys::KeyOutcome::Continue = outcome {
                    continue;
                }
            }
        }

        // Drain channel messages
        while let Ok(msg) = scan_rx.try_recv() {
            // Every scan message updates app state (session list, projects,
            // usage, status line, …), so a drained message means repaint.
            dirty = true;
            apply_scan_msg(
                &mut app,
                msg,
                &inflight_titles,
                &active_titles,
                &inflight_task_titles,
                &active_task_titles,
                &title_gate,
            );
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
                spawn_dispatch(
                    scan_tx_main.clone(),
                    tmux.clone(),
                    prompt,
                    format!("dispatched queued prompt to [{}]", tmux),
                    "queued dispatch failed".to_string(),
                );
                dirty = true;
            }
            app::DispatchAction::Timeout { tmux } => {
                log::warn!("dispatch: pending target [{}] never became idle", tmux);
                app.set_status(format!(
                    "queued dispatch timed out — [{}] never became idle",
                    tmux
                ));
                dirty = true;
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
