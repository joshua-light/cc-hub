mod acks;
mod app;
mod conversation;
mod focus;
mod models;
mod live_view;
mod scanner;
mod spawn;
mod ui;

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
}

fn init_logging() -> PathBuf {
    let log_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("cc-hub");
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

    // Scanner task — interval fires immediately on first tick, so no separate initial scan needed
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        let mut latest_sessions: Vec<models::SessionInfo> = Vec::new();

        loop {
            tokio::select! {
                _ = interval.tick() => {
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

        terminal.draw(|frame| ui::render(frame, &mut app))?;

        let poll_ms = 50;

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
                    (View::Grid, KeyCode::Char('f')) => {
                        if let Some(session) = app.selected_session_info() {
                            focus::focus_window(session.pid);
                        }
                    }
                    // Space: ack the selected WaitingForInput session until new activity.
                    (View::Grid, KeyCode::Char(' ')) => {
                        app.ack_selected();
                    }
                    // 'n' spawns a new ccyo session. Uses the selected session's cwd
                    // and pid — the pid is used to place the new window on the same
                    // Hyprland workspace.
                    (View::Grid, KeyCode::Char('n')) => {
                        if let Some(sess) = app.selected_session_info().cloned() {
                            match spawn::spawn_claude_session(&sess.cwd, Some(sess.pid)) {
                                Ok(msg) => app.set_status(msg),
                                Err(e) => app.set_status(format!("spawn failed: {}", e)),
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
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}
