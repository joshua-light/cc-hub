mod acks;
mod app;
mod conversation;
mod focus;
mod folder_picker;
mod models;
mod live_view;
mod platform;
mod scanner;
mod send;
mod spawn;
mod tmux_pane;
mod ui;
mod usage;
mod watcher;

use app::{App, View};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use log::LevelFilter;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use simplelog::{Config as LogConfig, WriteLogger};
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

enum ScanMsg {
    SessionList(Vec<models::SessionInfo>),
    Detail(models::SessionDetail),
    StateDebug(models::SessionInfo, conversation::StateExplanation),
    Usage(usage::UsageInfo),
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

#[tokio::main]
async fn main() -> io::Result<()> {
    if std::env::args().any(|a| a == "--no-tui") {
        return run_no_tui();
    }

    let log_path = init_logging();

    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal).await;

    terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
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

    let (scan_tx, mut scan_rx) = mpsc::channel::<ScanMsg>(16);
    let (detail_tx, mut detail_rx) = mpsc::channel::<String>(4);
    let (state_debug_tx, mut state_debug_rx) = mpsc::channel::<String>(4);

    let usage_tx = scan_tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
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
        let mut fallback = tokio::time::interval(Duration::from_secs(2));
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

        terminal.draw(|frame| ui::render(frame, &mut app))?;

        let poll_ms = if app.view == View::TmuxPane { 16 } else { 50 };

        if event::poll(Duration::from_millis(poll_ms))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match (&app.view, key.code) {
                    // Quit
                    (View::Grid, KeyCode::Char('q')) => {
                        app.should_quit = true;
                    }
                    // Grid navigation
                    (View::Grid, KeyCode::Right | KeyCode::Char('l')) => app.move_right(),
                    (View::Grid, KeyCode::Left | KeyCode::Char('h')) => app.move_left(),
                    (View::Grid, KeyCode::Down | KeyCode::Char('j')) => app.move_down(),
                    (View::Grid, KeyCode::Up | KeyCode::Char('k')) => app.move_up(),
                    // Enter: open live tail view
                    (View::Grid, KeyCode::Enter) => {
                        if let Some(session) = app.selected_session_info().cloned() {
                            if let Some(path) = session.jsonl_path.clone() {
                                let lv = live_view::LiveView::new(path);
                                app.enter_live_tail(lv);
                            } else {
                                // No JSONL file, fall back to info popup
                                let _ = detail_tx.send(session.session_id.clone()).await;
                                app.enter_popup();
                            }
                        }
                    }
                    // 'i' for info popup (old Enter behavior)
                    (View::Grid, KeyCode::Char('i')) => {
                        if let Some(id) = app.selected_session_id() {
                            let _ = detail_tx.send(id).await;
                            app.enter_popup();
                        }
                    }
                    (View::Grid, KeyCode::Char('D')) => {
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
                    (View::Grid, KeyCode::Char('f')) => {
                        if let Some(session) = app.selected_session_info().cloned() {
                            if let Some(tmux_name) = session.tmux_session.clone() {
                                // Size: frame inner minus a generous margin. The
                                // renderer re-resizes on first draw anyway, so a
                                // rough starting size is fine.
                                let (cols, rows) = terminal.size()
                                    .map(|s| (s.width.saturating_sub(6).max(20), s.height.saturating_sub(6).max(10)))
                                    .unwrap_or((120, 30));
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
                    (View::TmuxPane, KeyCode::F(1)) => {
                        app.close_tmux_pane();
                    }
                    (View::TmuxPane, _) => {
                        if let Some(pane) = app.tmux_pane.as_mut() {
                            pane.send_key(key);
                        }
                    }
                    (View::Grid, KeyCode::Char('x')) => {
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
                    (View::Grid, KeyCode::Char(' ')) => {
                        app.ack_selected();
                    }
                    (View::Grid, KeyCode::Char('n')) => {
                        if let Some(sess) = app.selected_session_info().cloned() {
                            let status = match spawn::spawn_claude_session(&sess.cwd) {
                                Ok(name) => format!("started ccyo [{}]", name),
                                Err(e) => format!("spawn failed: {}", e),
                            };
                            app.set_status(status);
                        }
                    }
                    (View::Grid, KeyCode::Char('N')) => {
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
                    (View::FolderPicker, KeyCode::Char('s') | KeyCode::Char(' ')) => {
                        let cwd = app
                            .folder_picker
                            .as_ref()
                            .map(|p| p.current_dir.display().to_string());
                        app.close_folder_picker();
                        if let Some(cwd) = cwd {
                            let status = match spawn::spawn_claude_session(&cwd) {
                                Ok(name) => format!("started ccyo [{}]", name),
                                Err(e) => format!("spawn failed: {}", e),
                            };
                            app.set_status(status);
                        }
                    }
                    (View::Grid, KeyCode::Char('p')) => {
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
                        match spawn::spawn_claude_session(&cwd) {
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
                ScanMsg::SessionList(sessions) => app.update_sessions(sessions),
                ScanMsg::Detail(detail) => app.update_detail(detail),
                ScanMsg::StateDebug(info, exp) => {
                    let lines = ui::build_state_debug_content(&info, &exp);
                    app.update_state_debug(info, exp, lines);
                }
                ScanMsg::Usage(u) => {
                    let line = ui::build_usage_line(&u);
                    app.update_usage(u, line);
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
