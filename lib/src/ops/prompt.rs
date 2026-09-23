//! Prompt delivery into a freshly spawned session: wait until the agent is
//! ready for input, then send the prompt.

use std::time::{Duration, Instant};

use crate::models;
use crate::{scanner, send};

/// Cold claude sessions in fresh cwds (no JSONL history, no trust-store
/// entry) take longer to reach Idle than warm dev directories. 120s leaves
/// margin even for the slowest path; the timeout exists to surface
/// genuinely broken spawns, not to bound happy-path latency.
pub const DEFAULT_PROMPT_WAIT_SECS: u64 = 120;

fn find_by_tmux<'a>(
    sessions: &'a [models::SessionInfo],
    tmux: &str,
) -> Option<&'a models::SessionInfo> {
    sessions
        .iter()
        .find(|s| s.tmux_session.as_deref() == Some(tmux))
}

/// Block until `tmux_name` reaches a prompt-ready state, then send `prompt`.
///
/// Layered readiness, same shape as App::poll_pending_dispatch:
///   1. scanner Idle + pane shows claude's empty `❯` input row. Tightest,
///      preferred.
///   2. scanner Idle + >=5s elapsed. Fallback for the case where claude
///      renders something we don't recognise; without it, any cosmetic
///      mismatch silently drops the prompt at the timeout boundary.
pub fn wait_until_idle_and_send(
    tmux_name: &str,
    prompt: &str,
    timeout: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    let deadline = started + timeout;
    loop {
        let sessions = scanner::scan_sessions();
        let scanner_idle = find_by_tmux(&sessions, tmux_name)
            .is_some_and(|s| s.state == models::SessionState::Idle);
        if scanner_idle {
            let pane_ready = send::pane_ready_for_input(tmux_name);
            let aged_in = started.elapsed() >= Duration::from_secs(5);
            if pane_ready || aged_in {
                if !pane_ready {
                    log::info!(
                        "dispatch: pane_ready=false but {}s elapsed — sending anyway (target=[{}])",
                        started.elapsed().as_secs(),
                        tmux_name
                    );
                }
                return send::send_prompt(tmux_name, prompt)
                    .map_err(|e| format!("send_prompt: {}", e));
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{} did not become ready within {}s",
                tmux_name,
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Outcome of dispatching a prompt to a freshly spawned session.
pub enum PromptStatus {
    /// The prompt was sent (or the agent accepted it as an initial prompt).
    Sent,
    /// No prompt was supplied — nothing to dispatch.
    Skipped,
    /// Dispatch failed; the session is up but the prompt could not be
    /// delivered. Carries the human warning string the caller should
    /// surface on stderr.
    Deferred(String),
}

impl PromptStatus {
    /// The stable string used in the `prompt_status` JSON field.
    pub fn as_str(&self) -> &'static str {
        match self {
            PromptStatus::Sent => "sent",
            PromptStatus::Skipped => "skipped",
            PromptStatus::Deferred(_) => "deferred",
        }
    }
}
