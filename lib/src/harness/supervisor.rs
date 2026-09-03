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
        let state = super::load_state(&dir);
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
