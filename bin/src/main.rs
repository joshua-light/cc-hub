#![allow(clippy::collapsible_match)]

use cc_hub_lib::{
    app, auto_review, config, conversation, harness, metrics, models, platform, projects_scan,
    scanner, send, session_count, spawn, title, triage, ui, usage, watcher,
};

use app::{App, Tab, View};

mod cli;
mod effects;
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
    Event, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use log::LevelFilter;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use simplelog::WriteLogger;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

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

/// The concrete terminal type after the byte-counting writer was threaded
/// under the backend — one alias so `run()`, `keys::handle_key`, and
/// `effects::apply_effect` don't each spell the full generic stack.
pub(crate) type Term = Terminal<CrosstermBackend<CountingWriter<io::Stdout>>>;

/// Log the 1/5/15-minute load averages. Input-lag forensics repeatedly dead-end
/// at "the app was fast, the delay was outside the process" — this line ties
/// each felt incident to how loaded the whole machine was at that moment.
#[cfg(unix)]
fn log_loadavg() {
    let mut la = [0f64; 3];
    // SAFETY: `la` is a valid buffer of 3 doubles and getloadavg writes at
    // most the 3 requested entries.
    let n = unsafe { libc::getloadavg(la.as_mut_ptr(), 3) };
    if n == 3 {
        log::debug!("sys: loadavg={:.2} {:.2} {:.2}", la[0], la[1], la[2]);
    }
}

#[cfg(not(unix))]
fn log_loadavg() {}

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
                        project_id_for_persist.as_deref(),
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
    /// The full transcript archive for the session finder, built off the
    /// event loop by [`Effect::BuildSessionIndex`].
    SessionIndex(Vec<cc_hub_lib::session_index::IndexedSession>),
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
    /// Fresh on-disk snapshot of every persistent agent (Agents tab).
    Harness(Vec<harness::AgentSnapshot>),
    /// A persistent agent finished a tick; carries its status-bar line.
    HarnessTick(harness::supervisor::TickReport),
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
    if app.tasks.pending_assign.is_some() {
        let status = app.assign_task_agent(cwd);
        app.set_status(status);
    } else if app.projects.registering_only {
        let status = match app.register_picked_project(cwd) {
            Ok(name) => format!("registered project: {}", name),
            Err(e) => format!("register failed: {}", e),
        };
        app.set_status(status);
    } else if app.projects.creating_task {
        app.enter_project_task_prompt(cwd.to_string());
    } else {
        app.close_folder_picker();
        let agent_id = app.default_session_agent_id().to_string();
        let status = match spawn::spawn_agent_session(&agent_id, cwd, None, None, None, false) {
            Ok(name) => {
                let status = format!("started {} [{}]", agent_id, name);
                app.watch_spawn(name, agent_id, cwd.to_string());
                status
            }
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
pub(crate) fn popup_pane_size(terminal: &Term) -> (u16, u16) {
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
        // Millisecond timestamps: input-latency forensics correlate a key's
        // arrival with the frame that follows — second granularity can't
        // show a 200ms stall. (Times are UTC; simplelog's local offset
        // needs an extra feature + unsound-on-unix caveats.)
        let config = simplelog::ConfigBuilder::new()
            .set_time_format_custom(time::macros::format_description!(
                "[hour]:[minute]:[second].[subsecond digits:3]"
            ))
            .build();
        WriteLogger::init(LevelFilter::Debug, config, file).ok();
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

/// Pull a global `--claude-config-dir <path>` (or `--claude-config-dir=<path>`)
/// out of argv and export it as `CLAUDE_CONFIG_DIR` for this process. Setting
/// the env var — rather than threading a value through — means the directly
/// spawned `claude -p` helpers (titles, backlog, auto-review) inherit the right
/// account for free; mux-spawned sessions get it re-applied explicitly in
/// `spawn`. Returns argv with the flag (and its value) removed so per-verb flag
/// parsers don't choke on it.
fn extract_claude_config_dir(argv: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len());
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        let dir = if let Some(v) = a.strip_prefix("--claude-config-dir=") {
            Some(v.to_string())
        } else if a == "--claude-config-dir" {
            let v = argv.get(i + 1).cloned();
            i += 1; // also skip the value
            v
        } else {
            out.push(a.clone());
            i += 1;
            continue;
        };
        if let Some(d) = dir {
            std::env::set_var("CLAUDE_CONFIG_DIR", expand_tilde(&d));
        }
        i += 1;
    }
    out
}

/// Expand a leading `~/` (or bare `~`) to `$HOME`. Shells already do this for
/// unquoted args, but a quoted/scripted value reaches us literally.
fn expand_tilde(path: &str) -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return path.to_string();
    };
    let home = home.to_string_lossy();
    if path == "~" {
        return home.into_owned();
    }
    match path.strip_prefix("~/") {
        Some(rest) => format!("{}/{}", home.trim_end_matches('/'), rest),
        None => path.to_string(),
    }
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let argv = extract_claude_config_dir(argv);
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
    // Frame-diff byte counter, drained once per draw by `run()` for the
    // `bytes=` field of the draw trace.
    let frame_bytes = Arc::new(AtomicU64::new(0));
    let backend = CrosstermBackend::new(CountingWriter::new(stdout, Arc::clone(&frame_bytes)));
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, image_picker, frame_bytes).await;

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
///
/// Returns true when the message changed anything the renderer shows. The
/// periodic scan arms (`SessionList` / `Projects`) report no-change for the
/// common all-idle tick so the caller can skip the repaint; every other
/// message exists only to mutate visible state, so they always return true.
fn apply_scan_msg(
    app: &mut App,
    msg: ScanMsg,
    inflight_titles: &Arc<Mutex<HashMap<String, Instant>>>,
    active_titles: &Arc<Mutex<HashSet<String>>>,
    inflight_task_titles: &Arc<Mutex<HashMap<String, Instant>>>,
    active_task_titles: &Arc<Mutex<HashSet<String>>>,
    title_gate: &Arc<tokio::sync::Semaphore>,
) -> bool {
    match msg {
        ScanMsg::SessionList(mut sessions) => {
            queue_missing_titles(&mut sessions, inflight_titles, active_titles, title_gate);
            return app.update_sessions(sessions);
        }
        ScanMsg::SessionIndex(index) => app.update_session_index(index),
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
            queue_missing_task_titles(
                &mut snap,
                inflight_task_titles,
                active_task_titles,
                title_gate,
            );
            return app.update_projects(snap);
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
        ScanMsg::Harness(agents) => app.update_harness(agents),
        ScanMsg::HarnessTick(report) => {
            if !report.ok {
                app.set_status(report.status);
            } else {
                log::info!("harness: {}", report.status);
            }
        }
        ScanMsg::DispatchResult { ok } => {
            // Success and failure both already hold the rendered
            // status line; either way it goes straight to the bar.
            app.set_status(ok.unwrap_or_else(|e| e));
        }
    }
    true
}

async fn run(
    terminal: &mut Term,
    image_picker: cc_hub_lib::ratatui_image::picker::Picker,
    frame_bytes: Arc<AtomicU64>,
) -> io::Result<()> {
    // Migrate a pre-unification ~/.cc-hub/tasks.json into the per-task store
    // BEFORE the board's first load. Deliberately here, not in App::new():
    // this is the one destructive startup step, and App::new() also runs in
    // tests and hot-reload paths that must never touch the real home.
    let migration_error = match cc_hub_lib::tasks::migrate_legacy_board() {
        Ok(Some(n)) => {
            log::info!("task board: migrated {} task(s) from tasks.json", n);
            None
        }
        Ok(None) => None,
        Err(e) => {
            log::error!("task board migration failed: {}", e);
            Some(format!("task board migration failed: {e}"))
        }
    };

    let mut app = App::new();
    // Swap in the persisted ack tracker so Space-idled cards survive a
    // restart. Same rationale as the migration above for living here and not
    // in App::new(): tests and hot-reload paths must never touch the real
    // home, so App::new() constructs a purely in-memory tracker.
    app.sessions.acks = cc_hub_lib::acks::Acks::load();
    app.image_picker = Some(image_picker);
    if let Some(msg) = migration_error {
        app.tasks.persistence_error = Some(msg);
    }

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

    // Persistent agents. The supervisor loops live in this process (one
    // tokio task per agent dir); the TUI reads their state back from disk on
    // a short timer and right after every tick report, so CLI-side changes
    // (poke, pause, notes) show up too.
    {
        let (tick_tx, mut tick_rx) = mpsc::channel::<harness::supervisor::TickReport>(16);
        let supervisor_on = config::get().harness.enabled;
        if supervisor_on {
            let _manager = harness::supervisor::spawn(tick_tx);
        }
        app.harness.supervisor_on = supervisor_on;
        let harness_tx = scan_tx.clone();
        tokio::spawn(async move {
            let mut refresh = tokio::time::interval(config::get().harness.refresh());
            refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = refresh.tick() => {}
                    report = tick_rx.recv() => {
                        match report {
                            Some(r) => {
                                let _ = harness_tx.send(ScanMsg::HarnessTick(r)).await;
                            }
                            None => {
                                // Supervisor off: keep refreshing for CLI-driven agents.
                                refresh.tick().await;
                            }
                        }
                    }
                }
                if !harness::root_exists() {
                    continue;
                }
                let agents = tokio::task::spawn_blocking(harness::scan)
                    .await
                    .unwrap_or_default();
                let _ = harness_tx.send(ScanMsg::Harness(agents)).await;
            }
        });
    }

    // Fallback timer catches PID deaths (not a filesystem event) and events
    // missed when a watched dir is rotated or recreated. Its initial tick
    // fires immediately, serving as the startup scan.
    let (watch_tx, mut watch_rx) = mpsc::channel::<watcher::WatchBatch>(8);
    watcher::spawn_fs_watcher(watch_tx);
    let (session_invalidate_tx, mut session_invalidate_rx) = mpsc::channel::<()>(1);
    let (project_invalidate_tx, mut project_invalidate_rx) = mpsc::channel::<()>(1);
    tokio::spawn(async move {
        while let Some(mut batch) = watch_rx.recv().await {
            while let Ok(next) = watch_rx.try_recv() {
                batch.sessions |= next.sessions;
                batch.projects |= next.projects;
            }
            if batch.sessions {
                let _ = session_invalidate_tx.try_send(());
            }
            if batch.projects {
                let _ = project_invalidate_tx.try_send(());
            }
        }
    });

    let session_scan_tx = scan_tx.clone();
    tokio::spawn(async move {
        let mut fallback = tokio::time::interval(config::get().scan.fs_fallback_interval());
        fallback.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut full_reconcile = tokio::time::interval(Duration::from_secs(10));
        full_reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Consume the full interval's immediate tick; the fallback branch owns
        // the startup scan, then full missed-event recovery runs every 10s.
        full_reconcile.tick().await;
        let mut latest_sessions: Vec<models::SessionInfo> = Vec::new();

        loop {
            tokio::select! {
                _ = fallback.tick() => {
                    if latest_sessions.is_empty() {
                        let sessions = tokio::task::spawn_blocking(scanner::scan_sessions)
                            .await
                            .unwrap_or_default();
                        latest_sessions = sessions.clone();
                        let _ = session_scan_tx.send(ScanMsg::SessionList(sessions)).await;
                    } else if scanner::refresh_process_liveness(&mut latest_sessions) {
                        let _ = session_scan_tx
                            .send(ScanMsg::SessionList(latest_sessions.clone()))
                            .await;
                    }
                }
                _ = full_reconcile.tick() => {
                    let sessions = tokio::task::spawn_blocking(scanner::scan_sessions)
                        .await
                        .unwrap_or_default();
                    latest_sessions = sessions.clone();
                    let _ = session_scan_tx.send(ScanMsg::SessionList(sessions)).await;
                }
                Some(()) = session_invalidate_rx.recv() => {
                    let sessions = tokio::task::spawn_blocking(scanner::scan_sessions)
                        .await
                        .unwrap_or_default();
                    latest_sessions = sessions.clone();
                    let _ = session_scan_tx.send(ScanMsg::SessionList(sessions)).await;
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
                        let _ = session_scan_tx.send(ScanMsg::Detail(d)).await;
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
                        let _ = session_scan_tx.send(ScanMsg::StateDebug(info, e)).await;
                    }
                }
            }
        }
    });

    // Project state has its own invalidation and blocking worker. A large
    // project scan can no longer delay Sessions refreshes or detail requests.
    let project_scan_tx = scan_tx.clone();
    tokio::spawn(async move {
        let project_fallback = config::get()
            .scan
            .fs_fallback_interval()
            .max(Duration::from_secs(10));
        let mut fallback = tokio::time::interval(project_fallback);
        fallback.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = fallback.tick() => {}
                Some(()) = project_invalidate_rx.recv() => {}
            }
            let snap = tokio::task::spawn_blocking(projects_scan::scan)
                .await
                .unwrap_or_else(|_| projects_scan::ProjectsSnapshot::empty());
            let _ = project_scan_tx.send(ScanMsg::Projects(snap)).await;
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

    // Redraw gating (issue #18a). The loop wakes every ~5ms but the widget
    // tree only changes on input, on a drained ScanMsg, on a LiveTail poll
    // that picked up new entries, or on the once-per-second elapsed clock.
    // We draw immediately on the first three (so input latency is unchanged)
    // and throttle the purely time-driven "refreshed Ns ago" / elapsed-card
    // repaint to ~1Hz. `dirty` starts true so the first frame always renders.
    // The embedded tmux pane is the exception: its content is fed by a
    // background reader, so while it's open we redraw every loop tick.
    let mut dirty = true;
    let mut last_clock_redraw = Instant::now();
    // Set when an input event is handled, consumed by the draw that follows:
    // the resulting `latency=` in the draw trace is the app-side time from
    // event read to frame flushed. If a user-felt lag isn't visible here,
    // the time is being lost outside the process (terminal, compositor).
    let mut last_input_at: Option<Instant> = None;
    /// Any single loop phase taking this long is a responsiveness incident
    /// worth a warn-level breakdown in the log.
    const STALL: Duration = Duration::from_millis(30);
    /// Minimum spacing between terminal round-trip probes, so a key mash
    /// costs at most one probe every 2s.
    const PROBE_INTERVAL: Duration = Duration::from_secs(2);

    // Terminal round-trip probe state. In-process telemetry keeps proving
    // the loop innocent while users still feel 500-1000ms on a keypress, so
    // after an input-triggered frame is flushed we ask the terminal where
    // its cursor is (CPR, ESC[6n) and time the answer. The reply can only
    // arrive after the terminal has consumed everything we wrote, so the
    // round trip exposes a backlogged terminal/tmux/ssh hop — the part of
    // the pipeline no in-process timer can see. One failed probe disables
    // probing for the rest of the run: a terminal that doesn't answer CPR
    // would otherwise cost a 2s blocking timeout per probe.
    let mut probe_ok = true;
    let mut last_probe = Instant::now();
    // The pane redraws at stream rate and its per-frame trace is skipped to
    // keep the log readable; aggregate its flushed bytes and report ~1Hz so
    // pane-driven terminal load still shows up in forensics.
    let mut pane_bytes: u64 = 0;
    let mut last_pane_bytes_log = Instant::now();
    let mut last_sys_log = Instant::now();

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
        let t_draw = Instant::now();
        let mut draw_dur = Duration::ZERO;
        if dirty || in_tmux || clock_tick {
            terminal.draw(|frame| hot::render(frame, &mut app))?;
            draw_dur = t_draw.elapsed();
            // Diff size this frame pushed at the terminal. Drained even for
            // pane frames so their bytes can't leak into the next Grid
            // frame's number.
            let flushed = frame_bytes.swap(0, Ordering::Relaxed);
            let input_latency = if in_tmux {
                None
            } else {
                last_input_at.take().map(|t| t.elapsed())
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
                    if dirty { "dirty" } else { "clock" },
                    draw_dur,
                    flushed,
                    input_latency,
                );
            } else {
                pane_bytes = pane_bytes.saturating_add(flushed);
                if last_pane_bytes_log.elapsed() >= Duration::from_secs(1) {
                    log::debug!(
                        "draw: pane flushed {} bytes in {:?}",
                        pane_bytes,
                        last_pane_bytes_log.elapsed()
                    );
                    pane_bytes = 0;
                    last_pane_bytes_log = Instant::now();
                }
            }
            dirty = false;
            last_clock_redraw = Instant::now();

            // Terminal round-trip probe (see `probe_ok` above): fired only
            // right after a keypress-triggered frame, at most once per
            // PROBE_INTERVAL. A slow answer here with a fast `latency=` on
            // the same frame localizes the felt lag to the terminal side.
            if probe_ok && input_latency.is_some() && last_probe.elapsed() >= PROBE_INTERVAL {
                last_probe = Instant::now();
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
                        probe_ok = false;
                        log::warn!("probe: CPR failed ({}); probing disabled for this run", e);
                    }
                }
            }
        }

        // Replay clipboard escapes (OSC 52) captured from the embedded pane
        // onto the real terminal. tmux addresses the escape to its attach
        // client — the pane's pty, where the vt100 parser would drop it —
        // so this hop is what carries an in-pane copy to the viewer's
        // terminal when cc-hub runs on a remote box over ssh. Done here,
        // between frames on the render thread, so a replay can never tear
        // a ratatui write.
        if let Some(pane) = app.tmux_pane.as_ref() {
            for seq in pane.take_osc52() {
                let backend = terminal.backend_mut();
                if let Err(e) = backend.write_all(&seq).and_then(|()| backend.flush()) {
                    log::warn!("osc52 replay failed: {}", e);
                }
            }
        }

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
                dirty = true;
                last_input_at = Some(Instant::now());
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
                            let on_sessions =
                                app.view == View::Grid && app.current_tab == Tab::Sessions;
                            let on_metrics =
                                app.view == View::Grid && app.current_tab == Tab::Metrics;
                            let on_projects =
                                app.view == View::Grid && app.current_tab == Tab::Projects;
                            let on_tasks = app.view == View::Grid && app.current_tab == Tab::Tasks;
                            let on_agents =
                                app.view == View::Grid && app.current_tab == Tab::Agents;

                            let sel_before = (app.sessions.sel_group, app.sessions.sel_in_group);
                            // KeyOutcome::Continue used to skip this pass's
                            // scan drain; with the whole burst handled before
                            // a single drain, both outcomes proceed
                            // identically here.
                            let _ = keys::handle_key(
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
                                on_tasks,
                                on_agents,
                            )
                            .await;
                            let sel_after = (app.sessions.sel_group, app.sessions.sel_in_group);
                            if sel_before != sel_after {
                                log::debug!("key: selection {:?} -> {:?}", sel_before, sel_after);
                            }
                        }
                    }
                    other => {
                        // Resize / focus events: nothing to do beyond the
                        // repaint, but keep them visible in the trace.
                        log::debug!("event: {:?}", other);
                    }
                }
                if app.should_quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
            input_dur = t_input.elapsed();
        }

        // Drain channel messages. Repaint only when a message actually
        // changed visible state — the periodic scan ticks usually carry an
        // identical snapshot, and skipping those keeps an unchanged grid
        // (and its selection) untouched between real changes.
        let t_drain = Instant::now();
        while let Ok(msg) = scan_rx.try_recv() {
            if apply_scan_msg(
                &mut app,
                msg,
                &inflight_titles,
                &active_titles,
                &inflight_task_titles,
                &active_task_titles,
                &title_gate,
            ) {
                dirty = true;
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
        let dispatch_dur = t_dispatch.elapsed();

        // Periodic machine-load line: felt lag with clean loop phases and a
        // clean probe points at the OS/emulator being starved — this ties
        // each incident window to the load averages at that moment.
        if last_sys_log.elapsed() >= Duration::from_secs(10) {
            last_sys_log = Instant::now();
            log_loadavg();
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

/// `HOME` and `CLAUDE_CONFIG_DIR` are process-global, so every test that mutates
/// them — wherever it lives in this binary — must serialize on this one lock.
/// A per-module lock let `cli`'s HOME-redirecting tests race the env-mutating
/// tests here, poisoning the lock and cascading `PoisonError` under
/// `cargo test --workspace`.
#[cfg(test)]
pub(crate) static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn with_clean_env<F: FnOnce()>(f: F) {
        let _guard = crate::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev_cfg = std::env::var_os("CLAUDE_CONFIG_DIR");
        let prev_home = std::env::var_os("HOME");
        std::env::remove_var("CLAUDE_CONFIG_DIR");
        f();
        match prev_cfg {
            Some(v) => std::env::set_var("CLAUDE_CONFIG_DIR", v),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn strips_space_separated_flag_and_sets_env() {
        with_clean_env(|| {
            let rest = extract_claude_config_dir(argv(&[
                "--claude-config-dir",
                "/tmp/acct",
                "spawn-worker",
                "--task",
                "t1",
            ]));
            assert_eq!(rest, argv(&["spawn-worker", "--task", "t1"]));
            assert_eq!(std::env::var("CLAUDE_CONFIG_DIR").unwrap(), "/tmp/acct");
        });
    }

    #[test]
    fn strips_equals_form_flag() {
        with_clean_env(|| {
            let rest = extract_claude_config_dir(argv(&["--claude-config-dir=/tmp/b", "task"]));
            assert_eq!(rest, argv(&["task"]));
            assert_eq!(std::env::var("CLAUDE_CONFIG_DIR").unwrap(), "/tmp/b");
        });
    }

    #[test]
    fn no_flag_leaves_argv_and_env_untouched() {
        with_clean_env(|| {
            let rest = extract_claude_config_dir(argv(&["worker", "list"]));
            assert_eq!(rest, argv(&["worker", "list"]));
            assert!(std::env::var_os("CLAUDE_CONFIG_DIR").is_none());
        });
    }

    #[test]
    fn expands_leading_tilde() {
        with_clean_env(|| {
            std::env::set_var("HOME", "/home/josh");
            extract_claude_config_dir(argv(&["--claude-config-dir", "~/.claude-personal"]));
            assert_eq!(
                std::env::var("CLAUDE_CONFIG_DIR").unwrap(),
                "/home/josh/.claude-personal"
            );
        });
    }
}
