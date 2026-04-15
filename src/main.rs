mod app;
mod conversation;
mod models;
mod scanner;
mod ui;

use app::{App, View};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::time::Duration;
use tokio::sync::mpsc;

enum ScanMsg {
    SessionList(Vec<models::SessionInfo>),
    Detail(models::SessionDetail),
}

#[tokio::main]
async fn main() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal).await;

    terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let mut app = App::new();

    let (scan_tx, mut scan_rx) = mpsc::channel::<ScanMsg>(16);
    let (detail_tx, mut detail_rx) = mpsc::channel::<String>(4);

    // Scanner task — interval fires immediately on first tick, so no separate initial scan needed
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
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
        terminal.draw(|frame| ui::render(frame, &mut app))?;

        if event::poll(Duration::from_millis(50))? {
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
                    (View::Grid, KeyCode::Enter) => {
                        if let Some(id) = app.selected_session_id() {
                            let _ = detail_tx.send(id).await;
                            app.enter_popup();
                        }
                    }
                    // Popup navigation
                    (View::Popup, KeyCode::Esc | KeyCode::Char('q')) => app.close_popup(),
                    (View::Popup, KeyCode::Down | KeyCode::Char('j')) => app.scroll_down(),
                    (View::Popup, KeyCode::Up | KeyCode::Char('k')) => app.scroll_up(),
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
