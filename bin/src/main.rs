use cc_hub_lib::sessions::scanner;
use cc_hub_lib::{models, title};
use std::io;

mod args;
mod cli;
mod effects;
mod event_loop;
mod keys;
mod logging;
mod scan_msg;
mod term;
mod titles;
mod workers;

#[tokio::main]
async fn main() -> io::Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let argv = args::extract_claude_config_dir(argv);
    if let Some(code) = cli::dispatch(&argv) {
        std::process::exit(code);
    }
    if argv.iter().any(|a| a == "--no-tui") {
        return run_no_tui();
    }

    let log_path = logging::init_logging();

    let (mut terminal, frame_bytes, modes) = term::enter()?;

    let result = event_loop::run(&mut terminal, frame_bytes).await;

    // Ask any still-running title subprocesses to kill themselves so the
    // tokio runtime's shutdown doesn't wait up to ~45s on a hung `claude
    // -p`. Blocking tasks can't be cancelled, but they poll this flag.
    title::request_shutdown();
    // Best-effort: flush the log backend so any warn lines emitted just
    // before exit make it to disk even if a panic-while-logging holds the
    // backend's mutex.
    log::logger().flush();

    term::leave(&mut terminal, &modes)?;

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

/// `HOME` and `CLAUDE_CONFIG_DIR` are process-global, so every test that mutates
/// them — wherever it lives in this binary — must serialize on this one lock.
/// A per-module lock let `cli`'s HOME-redirecting tests race the env-mutating
/// tests here, poisoning the lock and cascading `PoisonError` under
/// `cargo test --workspace`.
#[cfg(test)]
pub(crate) static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
