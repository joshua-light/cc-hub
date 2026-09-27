//! Prompts queued for freshly spawned sessions, sent once the target pane
//! is ready for input.

use super::App;
use crate::config;
use crate::models::SessionState;
use crate::send::Delivery;
use std::time::{Duration, Instant};

/// A prompt queued for a freshly-spawned tmux session that isn't yet Idle.
/// Drained by [`App::poll_pending_dispatch`] once the session shows up in the
/// next scan and its state flips to Idle, or times out after
/// [`config::UiConfig::pending_dispatch_timeout_secs`].
#[derive(Clone, Debug)]
pub struct PendingDispatch {
    tmux: String,
    prompt: String,
    delivery: Delivery,
    queued_at: Instant,
}

pub enum DispatchAction {
    Send {
        tmux: String,
        prompt: String,
        delivery: Delivery,
    },
    Timeout {
        tmux: String,
    },
    Wait,
}

impl App {
    pub fn queue_pending_dispatch(&mut self, tmux: String, prompt: String) {
        self.queue_delivery(tmux, prompt, Delivery::Submit);
    }

    /// Like [`Self::queue_pending_dispatch`], but the text is left typed in
    /// the input rather than submitted — a handoff's draft.
    pub fn queue_pending_draft(&mut self, tmux: String, draft: String) {
        self.queue_delivery(tmux, draft, Delivery::Draft);
    }

    fn queue_delivery(&mut self, tmux: String, prompt: String, delivery: Delivery) {
        self.pending_dispatch.push_back(PendingDispatch {
            tmux,
            prompt,
            delivery,
            queued_at: Instant::now(),
        });
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
        // Send once the scanner says Idle and either:
        //   1. the pane shows the agent's empty input row, so the paste is
        //      sure to land in the right place; or
        //   2. 5s have passed. The pane check can stay false when the agent
        //      renders something unrecognised (another glyph or theme), and
        //      without this fallback one cosmetic mismatch loses the prompt
        //      to the timeout, leaving an empty session.
        // Walk the unfiltered scan set, not `self.sessions.groups`: the
        // inactive filter can hide the very session to dispatch into.
        let scanner_idle = self.sessions.last_sessions.iter().any(|s| {
            s.tmux_session.as_deref() == Some(pd.tmux.as_str()) && s.state == SessionState::Idle
        });
        if scanner_idle {
            let aged_in = pd.queued_at.elapsed() >= Duration::from_secs(5);
            // The probe is a `tmux capture-pane` fork+exec, too costly for
            // every ~50ms frame, so it runs about twice a second; between
            // probes the pane counts as not ready. The `aged_in` fallback and
            // the timeout don't need the probe and still fire on schedule.
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
                    delivery: pd.delivery,
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

    /// How long the front dispatch has waited, for the status bar; `None`
    /// when nothing is queued.
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
            DispatchAction::Send { tmux, prompt, .. } => {
                assert_eq!(tmux, "tmux-a");
                assert_eq!(prompt, "prompt-a");
            }
            _ => panic!("first queued dispatch should send"),
        }
        match app.poll_pending_dispatch() {
            DispatchAction::Send { tmux, prompt, .. } => {
                assert_eq!(tmux, "tmux-b");
                assert_eq!(prompt, "prompt-b");
            }
            _ => panic!("second queued dispatch should send"),
        }
    }
}
