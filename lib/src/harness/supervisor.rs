//! The forever loop, one tokio task per agent, living inside the TUI
//! process. A manager task re-discovers agent dirs so a spec dropped in
//! while the hub runs starts without a restart, and each agent loop reloads
//! its spec every iteration so edits apply on the next tick.
//!
//! The trigger decides *when*; the loop decides *whether*. No event, no
//! tick, no spend. Every tick's outcome hits `state.json` before the next
//! one starts, so killing the hub mid-flight costs at most one tick.

use super::spec::TriggerKind;
use super::{trigger, Event, Spec};
use log::{info, warn};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// What an agent loop tells the TUI after each tick.
#[derive(Debug, Clone)]
pub struct TickReport {
    pub name: String,
    pub ok: bool,
    /// One line for the status bar.
    pub status: String,
}

const DISCOVER_EVERY: Duration = Duration::from_secs(5);
const IDLE_POLL: Duration = Duration::from_secs(1);
const PARKED_POLL: Duration = Duration::from_secs(3);
/// Grace period past `run.timeout_s` before a `ticking` marker is treated as
/// stale. The owning tick's own timeout already killed its child by then, so
/// a marker still standing after this means the process that set it — not
/// just the child — is gone (killed, crashed, machine slept).
const STALE_TICK_GRACE_S: i64 = 60;

/// Start the manager. Aborting the returned handle stops discovery; running
/// agent loops are aborted with it via the map it owns.
pub fn spawn(tx: mpsc::Sender<TickReport>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut loops: HashMap<PathBuf, JoinHandle<()>> = HashMap::new();
        loop {
            let dirs = tokio::task::spawn_blocking(super::agent_dirs)
                .await
                .unwrap_or_default();
            loops.retain(|dir, handle| {
                if dirs.contains(dir) && !handle.is_finished() {
                    true
                } else {
                    handle.abort();
                    info!("harness: stopped loop for {}", dir.display());
                    false
                }
            });
            for dir in dirs {
                if !loops.contains_key(&dir) {
                    info!("harness: starting loop for {}", dir.display());
                    let tx = tx.clone();
                    loops.insert(dir.clone(), tokio::spawn(agent_loop(dir, tx)));
                }
            }
            tokio::time::sleep(DISCOVER_EVERY).await;
        }
    })
}

async fn agent_loop(dir: PathBuf, tx: mpsc::Sender<TickReport>) {
    let inbox = super::inbox_path(&dir);
    {
        let inbox = inbox.clone();
        let _ = tokio::task::spawn_blocking(move || {
            trigger::ensure_inbox(&inbox)?;
            trigger::requeue_stale(&inbox)
        })
        .await;
    }
    // Interval/poll pacing lives here, not in state.json: a restart simply
    // fires the first poll straight away.
    let mut last_poll: Option<Instant> = None;
    let mut last_interval: Option<Instant> = None;
    let mut last_digest: Option<String> = None;
    let mut interval_n: u64 = 0;

    loop {
        let spec = match super::spec::load(&dir) {
            Ok(s) => s,
            Err(_) => {
                tokio::time::sleep(PARKED_POLL).await;
                continue;
            }
        };
        let state = reclaim_stale_tick(&dir, &spec).await;
        if !spec.enabled || state.paused || state.stopped_reason.is_some() {
            tokio::time::sleep(PARKED_POLL).await;
            continue;
        }
        if let Some(reason) = super::budget_block(&spec, &state) {
            halt(&dir, &spec, &reason, &tx).await;
            continue;
        }

        // Inbox first, for every trigger kind: a poke or an answer must not
        // wait behind a poll interval.
        let event = match next_event(
            &spec,
            &inbox,
            &mut last_poll,
            &mut last_interval,
            &mut last_digest,
            &mut interval_n,
        )
        .await
        {
            Some(ev) => ev,
            None => {
                tokio::time::sleep(IDLE_POLL).await;
                continue;
            }
        };

        let spec_for_tick = spec.clone();
        let ev_for_tick = event.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            super::tick_once(&spec_for_tick, Some(&ev_for_tick))
        })
        .await;
        let (tick, state) = match outcome {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                warn!("harness[{}]: tick failed to record: {}", spec.name, e);
                trigger::ack(&event, false);
                tokio::time::sleep(PARKED_POLL).await;
                continue;
            }
            Err(e) => {
                warn!("harness[{}]: tick task panicked: {}", spec.name, e);
                trigger::ack(&event, false);
                tokio::time::sleep(PARKED_POLL).await;
                continue;
            }
        };
        trigger::ack(&event, tick.ok);

        let status = if tick.ok {
            format!(
                "{}: tick #{} ok · {} turns · ${:.2}",
                spec.name, state.ticks, tick.turns, tick.cost_usd
            )
        } else {
            format!(
                "{}: tick #{} failed ({}) · {}",
                spec.name,
                state.ticks,
                tick.subtype.as_deref().unwrap_or("?"),
                super::truncate(&tick.result, 80)
            )
        };
        let _ = tx
            .send(TickReport {
                name: spec.name.clone(),
                ok: tick.ok,
                status,
            })
            .await;

        // Back off on failure so a broken agent doesn't burn budget at full
        // rate: interval × (2^n − 1), capped at n = 4.
        if state.failures_in_a_row > 0 && state.stopped_reason.is_none() {
            let n = state.failures_in_a_row.min(4);
            let penalty = spec.trigger.interval_s.max(5) * ((1u64 << n) - 1);
            tokio::time::sleep(Duration::from_secs(penalty.min(3600))).await;
        }
    }
}

/// A `ticking` marker outlives the process that set it if that process is
/// killed mid-tick — machine sleep, a forced quit, a crash — since only the
/// tick's own return path clears it. `age` is old enough once the tick's own
/// timeout (plus a grace period for it to notice and write the kill) has
/// passed: at that point either the owning process is still alive and about
/// to clear the marker itself, or it's gone and nothing else ever will.
fn tick_is_stale(since: i64, now: i64, timeout_s: u64) -> bool {
    now - since > timeout_s as i64 + STALE_TICK_GRACE_S
}

/// Called once per loop iteration: cheap, and self-resolving after the
/// first clear.
async fn reclaim_stale_tick(dir: &std::path::Path, spec: &Spec) -> super::AgentState {
    let state = super::load_state(dir);
    let Some(t) = &state.ticking else {
        return state;
    };
    if !tick_is_stale(t.since, super::now_unix(), spec.run.timeout_s) {
        return state;
    }
    warn!(
        "harness[{}]: clearing stale ticking state ({}s old, no owning process — supervisor likely restarted mid-tick)",
        spec.name,
        super::now_unix() - t.since
    );
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || super::update_state(&dir, |s| s.ticking = None))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(state)
}

#[cfg(test)]
mod tests {
    use super::tick_is_stale;

    #[test]
    fn fresh_tick_is_not_stale() {
        assert!(!tick_is_stale(1000, 1000 + 3600, 3600));
    }

    #[test]
    fn tick_within_grace_past_timeout_is_not_stale() {
        assert!(!tick_is_stale(1000, 1000 + 3600 + 60, 3600));
    }

    #[test]
    fn tick_past_timeout_and_grace_is_stale() {
        assert!(tick_is_stale(1000, 1000 + 3600 + 61, 3600));
    }
}

async fn halt(dir: &std::path::Path, spec: &Spec, reason: &str, tx: &mpsc::Sender<TickReport>) {
    let already = super::load_state(dir).stopped_reason.is_some();
    if !already {
        let _ = super::update_state(dir, |s| s.stopped_reason = Some(reason.to_string()));
        let _ = tx
            .send(TickReport {
                name: spec.name.clone(),
                ok: false,
                status: format!("{}: halted — {}", spec.name, reason),
            })
            .await;
    }
    tokio::time::sleep(PARKED_POLL).await;
}

async fn next_event(
    spec: &Spec,
    inbox: &std::path::Path,
    last_poll: &mut Option<Instant>,
    last_interval: &mut Option<Instant>,
    last_digest: &mut Option<String>,
    interval_n: &mut u64,
) -> Option<Event> {
    let inbox_owned = inbox.to_path_buf();
    if let Ok(Ok(Some(ev))) = tokio::task::spawn_blocking(move || trigger::take(&inbox_owned)).await
    {
        return Some(ev);
    }
    let every = Duration::from_secs(spec.trigger.interval_s.max(1));
    match spec.trigger.kind {
        TriggerKind::Inbox => None,
        TriggerKind::Poll => {
            if last_poll.is_some_and(|t| t.elapsed() < every) {
                return None;
            }
            *last_poll = Some(Instant::now());
            let command = spec.trigger.command.clone()?;
            let cwd = spec.dir.clone();
            let timeout = Duration::from_secs(spec.trigger.timeout_s);
            let out =
                tokio::task::spawn_blocking(move || trigger::run_poll(&command, &cwd, timeout))
                    .await
                    .ok()
                    .flatten()?;
            let digest = trigger::digest(&out);
            if spec.trigger.dedupe && last_digest.as_deref() == Some(&digest) {
                return None;
            }
            *last_digest = Some(digest.clone());
            Some(Event::synthetic(format!("poll-{}", digest), out, "poll"))
        }
        TriggerKind::Interval => {
            if last_interval.is_some_and(|t| t.elapsed() < every) {
                return None;
            }
            *last_interval = Some(Instant::now());
            *interval_n += 1;
            Some(Event::synthetic(
                format!("tick-{}", interval_n),
                "",
                "interval",
            ))
        }
    }
}
