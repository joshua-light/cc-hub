//! Prompts queued for freshly spawned sessions, sent once the target pane
//! is ready for input.

use super::App;
use crate::config;
use crate::models::SessionState;
use std::time::{Duration, Instant};

/// A prompt queued for a freshly-spawned tmux session that isn't yet Idle.
/// Drained by [`App::poll_pending_dispatch`] once the session shows up in the
/// next scan and its state flips to Idle, or times out after
/// [`config::UiConfig::pending_dispatch_timeout_secs`].
#[derive(Clone, Debug)]
pub struct PendingDispatch {
    tmux: String,
    prompt: String,
    queued_at: Instant,
}

pub enum DispatchAction {
    Send { tmux: String, prompt: String },
    Timeout { tmux: String },
    Wait,
}

impl App {
    pub fn queue_pending_dispatch(&mut self, tmux: String, prompt: String) {
        self.pending_dispatch.push_back(PendingDispatch {
            tmux,
            prompt,
            queued_at: Instant::now(),
        });
    }

    pub fn has_pending_dispatch(&self) -> bool {
        !self.pending_dispatch.is_empty()
    }

    pub fn pending_dispatch_count(&self) -> usize {
        self.pending_dispatch.len()
    }

    /// If a pending dispatch exists and the target session now reports Idle,
    /// consume it and return [`DispatchAction::Send`]. If the deadline has
    /// passed, return [`DispatchAction::Timeout`]. Otherwise, put it back and
    /// wait. Dispatches are FIFO so multiple Claude launches can't overwrite
    /// each other's initial prompts.
    pub fn poll_pending_dispatch(&mut self) -> DispatchAction {
        let Some(pd) = self.pending_dispatch.pop_front() else {
            return DispatchAction::Wait;
        };
        // Layered readiness, in order of preference:
        //   1. scanner says Idle AND pane shows claude's empty input row.
        //      Tightest gate — guarantees the next paste lands in the
        //      right place. Preferred when both signals agree.
        //   2. scanner says Idle AND we've waited long enough for cold
        //      boot (>5s). Fallback for the case where the pane-ready
        //      check stays false because claude is rendering something
        //      we don't recognise (different glyph, different theme).
        //      Without this, a single cosmetic mismatch loses the prompt
        //      to the timeout and the user sees a "session that just
        //      sits there empty" — the real-world failure mode that
        //      motivated this comment.
        // Walk the unfiltered scan set, not `self.sessions.groups`: the
        // Sessions view filter (inactive sessions) can hide the very session
        // we need to dispatch into.
        let scanner_idle = self.sessions.last_sessions.iter().any(|s| {
            s.tmux_session.as_deref() == Some(pd.tmux.as_str()) && s.state == SessionState::Idle
        });
        if scanner_idle {
            let aged_in = pd.queued_at.elapsed() >= Duration::from_secs(5);
            // The `pane_ready_for_input` probe is a `tmux capture-pane`
            // fork+exec — too costly to run every ~50ms render frame. Throttle
            // it to ~2x/sec; between probes treat the pane as not-yet-ready and
            // keep waiting. The cold-boot fallback (`aged_in`) and the timeout
            // below don't need the probe, so they still fire on schedule.
            let probe_due = self
                .last_dispatch_probe_at
                .is_none_or(|t| t.elapsed() >= Duration::from_millis(500));
            let pane_ready = if aged_in {
                // aged_in already sends; don't burn a probe.
                false
            } else if probe_due {
                self.last_dispatch_probe_at = Some(Instant::now());
                self.runtime.ready_for_input(&pd.tmux)
            } else {
                false
            };
            if pane_ready || aged_in {
                if !pane_ready {
                    log::info!(
                        "dispatch: pane_ready=false but {}s elapsed — sending anyway (target=[{}])",
                        pd.queued_at.elapsed().as_secs(),
                        pd.tmux
                    );
                }
                self.last_dispatch_probe_at = None;
                return DispatchAction::Send {
                    tmux: pd.tmux,
                    prompt: pd.prompt,
                };
            }
        }
        if pd.queued_at.elapsed() > config::get().ui.pending_dispatch_timeout() {
            self.last_dispatch_probe_at = None;
            return DispatchAction::Timeout { tmux: pd.tmux };
        }
        self.pending_dispatch.push_front(pd);
        DispatchAction::Wait
    }

    /// Time the current pending dispatch has been waiting. None when no
    /// dispatch is queued. Used by the status bar so the user can tell
    /// at a glance that a dispatch is still booting rather
    /// than wondering why nothing is happening.
    pub fn pending_dispatch_age(&self) -> Option<Duration> {
        self.pending_dispatch
            .front()
            .map(|pd| pd.queued_at.elapsed())
    }

    /// Tmux session name of the current pending dispatch, if any.
    pub fn pending_dispatch_target(&self) -> Option<&str> {
        self.pending_dispatch.front().map(|pd| pd.tmux.as_str())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::test_support::fake_session;

    #[test]
    fn pending_dispatch_is_fifo_queue() {
        let mut app = App::new();
        app.queue_pending_dispatch("tmux-a".into(), "prompt-a".into());
        app.queue_pending_dispatch("tmux-b".into(), "prompt-b".into());
        for pd in &mut app.pending_dispatch {
            pd.queued_at = Instant::now() - Duration::from_secs(6);
        }
        app.sessions.last_sessions = vec![
            fake_session("tmux-a", SessionState::Idle),
            fake_session("tmux-b", SessionState::Idle),
        ];

        match app.poll_pending_dispatch() {
            DispatchAction::Send { tmux, prompt } => {
                assert_eq!(tmux, "tmux-a");
                assert_eq!(prompt, "prompt-a");
            }
            _ => panic!("first queued dispatch should send"),
        }
        match app.poll_pending_dispatch() {
            DispatchAction::Send { tmux, prompt } => {
                assert_eq!(tmux, "tmux-b");
                assert_eq!(prompt, "prompt-b");
            }
            _ => panic!("second queued dispatch should send"),
        }
    }
}
