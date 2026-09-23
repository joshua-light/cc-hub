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
use crate::wake::{Stamp, Wake};
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
    let mut wakes: HashMap<String, Stamp> = HashMap::new();
    // The last spec error and poll failure written to the event log, so a
    // condition that persists is logged once, not every loop.
    let mut spec_error: Option<String> = None;
    let mut poll_error: Option<String> = None;

    loop {
        let spec = match super::spec::load(&dir) {
            Ok(s) => {
                if spec_error.take().is_some() {
                    super::log_event(&dir, "info", "agent.toml loads again");
                }
                s
            }
            Err(e) => {
                if spec_error.as_deref() != Some(e.as_str()) {
                    super::log_event(&dir, "error", format!("parked: {}", e));
                    spec_error = Some(e);
                }
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

        // A wake says only "look now" — the poll still decides whether
        // there is an event — so forgetting when it last ran is the whole
        // mechanism.
        if woken(&spec, &mut wakes) {
            last_poll = None;
            last_interval = None;
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
            &mut poll_error,
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
                super::log_event(&dir, "error", format!("run failed to record: {}", e));
                trigger::ack(&event, false);
                tokio::time::sleep(PARKED_POLL).await;
                continue;
            }
            Err(e) => {
                warn!("harness[{}]: tick task panicked: {}", spec.name, e);
                super::log_event(&dir, "error", format!("run crashed: {}", e));
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

/// True when a wake the spec subscribes to has happened since the last
/// look. The first look only records: a wake older than this loop is not
/// news, the same reason the task watcher ignores the board it starts with.
/// A name dropped from the spec is forgotten, so putting it back reads as
/// new again — cheap, and only ever one extra poll.
fn woken(spec: &Spec, seen: &mut HashMap<String, Stamp>) -> bool {
    seen.retain(|name, _| spec.trigger.wake.contains(name));
    let mut news = false;
    for name in &spec.trigger.wake {
        let Some(stamp) = Wake::named(name).and_then(|w| w.last()) else {
            continue;
        };
        match seen.get(name) {
            Some(&known) if known == stamp => {}
            Some(_) => news = true,
            None => {}
        }
        seen.insert(name.clone(), stamp);
    }
    news
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
    super::log_event(
        dir,
        "warn",
        "cleared a run left marked in flight — the hub likely quit or slept mid-run",
    );
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || super::update_state(&dir, |s| s.ticking = None))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(state)
}

async fn halt(dir: &std::path::Path, spec: &Spec, reason: &str, tx: &mpsc::Sender<TickReport>) {
    let already = super::load_state(dir).stopped_reason.is_some();
    if !already {
        let _ = super::update_state(dir, |s| s.stopped_reason = Some(reason.to_string()));
        super::log_event(dir, "error", format!("halted: {}", reason));
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
    poll_error: &mut Option<String>,
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
            let polled =
                tokio::task::spawn_blocking(move || trigger::run_poll(&command, &cwd, timeout))
                    .await
                    .ok()?;
            let out = match polled {
                Ok(out) => {
                    if poll_error.take().is_some() {
                        super::log_event(&spec.dir, "info", "poll works again");
                    }
                    out?
                }
                Err(e) => {
                    if poll_error.as_deref() != Some(e.as_str()) {
                        super::log_event(&spec.dir, "warn", e.clone());
                        *poll_error = Some(e);
                    }
                    return None;
                }
            };
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

// Unix-only: `with_temp_home` isolates by redirecting `$HOME`, which
// `dirs::home_dir()` honours on unix and ignores on Windows.
#[cfg(all(test, unix))]
mod tests {
    use super::{tick_is_stale, woken, Stamp, Wake};
    use crate::test_util::with_temp_home;
    use std::collections::HashMap;

    fn watching(name: &str) -> crate::harness::Spec {
        crate::harness::spec::parse(
            std::path::Path::new("/tmp/x"),
            &format!(
                "[trigger]\nkind = \"interval\"\nwake = [\"{}\"]\n[prompt]\ninstruction = \"x\"",
                name
            ),
        )
        .unwrap()
    }

    #[test]
    fn the_first_look_records_without_waking() {
        with_temp_home(|| {
            Wake::named("board").unwrap().now().unwrap();
            let spec = watching("board");
            let mut seen: HashMap<String, Stamp> = HashMap::new();

            assert!(!woken(&spec, &mut seen), "a wake older than the loop");
            assert!(!woken(&spec, &mut seen), "nothing happened since");

            Wake::named("board").unwrap().now().unwrap();
            assert!(woken(&spec, &mut seen), "touched since the last look");
            assert!(!woken(&spec, &mut seen), "and only once");
        });
    }

    #[test]
    fn a_wake_nobody_ever_touched_is_quiet() {
        with_temp_home(|| {
            let spec = watching("board");
            let mut seen: HashMap<String, Stamp> = HashMap::new();
            assert!(!woken(&spec, &mut seen));
            assert!(seen.is_empty());
        });
    }

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
