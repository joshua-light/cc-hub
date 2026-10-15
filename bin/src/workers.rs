//! Background tasks that feed the event loop. Each one does its blocking work
//! on the tokio blocking pool and reports back as a [`ScanMsg`].

use crate::scan_msg::ScanMsg;
use hal_lib::sessions::{count, scanner, watcher};
use hal_lib::{config, harness, metrics, models, send, usage};
use std::io;
use std::time::Duration;
use tokio::sync::mpsc;

/// Poll subscription usage and recent-session counts.
pub(crate) fn spawn_usage(usage_tx: mpsc::Sender<ScanMsg>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(config::get().scan.usage_refresh_interval());
        loop {
            interval.tick().await;
            let (usage_opt, counts) = tokio::task::spawn_blocking(|| {
                (usage::fetch_usage(), count::count_recent_sessions())
            })
            .await
            .unwrap_or((None, count::SessionCounts::default()));
            let _ = usage_tx.send(ScanMsg::SessionCounts(counts)).await;
            if let Some(u) = usage_opt {
                let _ = usage_tx.send(ScanMsg::Usage(u)).await;
            }
        }
    });
}

/// Persistent agents. The supervisor loops live in this process (one
/// tokio task per agent dir); the TUI reads their state back from disk on
/// a short timer and right after every tick report, so CLI-side changes
/// (poke, pause, notes) show up too. Returns whether the supervisor runs.
pub(crate) fn spawn_harness(harness_tx: mpsc::Sender<ScanMsg>) -> bool {
    let (tick_tx, mut tick_rx) = mpsc::channel::<harness::supervisor::TickReport>(16);
    let supervisor_on = config::get().harness.enabled;
    if supervisor_on {
        let _manager = harness::supervisor::spawn(tick_tx);
    }
    tokio::spawn(async move {
        let mut refresh = tokio::time::interval(config::get().harness.refresh());
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = refresh.tick() => {}
                report = tick_rx.recv() => {
                    match report {
                        Some(r) => {
                            let _ = harness_tx.send(ScanMsg::HarnessTick(r)).await;
                        }
                        None => {
                            // Supervisor off: keep refreshing for CLI-driven agents.
                            refresh.tick().await;
                        }
                    }
                }
            }
            if !harness::root_exists() {
                continue;
            }
            let agents = tokio::task::spawn_blocking(harness::scan)
                .await
                .unwrap_or_default();
            let _ = harness_tx.send(ScanMsg::Harness(agents)).await;
        }
    });
    supervisor_on
}

/// The session grid's scanner, and the detail loader that reads from its
/// latest scan.
///
/// Fallback timer catches PID deaths (not a filesystem event) and events
/// missed when a watched dir is rotated or recreated. Its initial tick
/// fires immediately, serving as the startup scan.
pub(crate) fn spawn_session_scanner(
    session_scan_tx: mpsc::Sender<ScanMsg>,
    mut detail_rx: mpsc::Receiver<String>,
) {
    let (session_invalidate_tx, mut session_invalidate_rx) = mpsc::channel::<()>(1);
    watcher::spawn_fs_watcher(session_invalidate_tx);

    tokio::spawn(async move {
        let mut fallback = tokio::time::interval(config::get().scan.fs_fallback_interval());
        fallback.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut full_reconcile = tokio::time::interval(Duration::from_secs(10));
        full_reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Consume the full interval's immediate tick; the fallback branch owns
        // the startup scan, then full missed-event recovery runs every 10s.
        full_reconcile.tick().await;
        let mut latest_sessions: Vec<models::SessionInfo> = Vec::new();

        loop {
            tokio::select! {
                _ = fallback.tick() => {
                    if latest_sessions.is_empty() {
                        full_scan(&session_scan_tx, &mut latest_sessions).await;
                    } else if scanner::refresh_process_liveness(&mut latest_sessions) {
                        let _ = session_scan_tx
                            .send(ScanMsg::SessionList(latest_sessions.clone()))
                            .await;
                    }
                }
                _ = full_reconcile.tick() => {
                    full_scan(&session_scan_tx, &mut latest_sessions).await;
                }
                Some(()) = session_invalidate_rx.recv() => {
                    full_scan(&session_scan_tx, &mut latest_sessions).await;
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
                        let _ = session_scan_tx.send(ScanMsg::Detail(d)).await;
                    }
                }
            }
        }
    });
}

/// Rescan every session, keep the result as the scanner's latest snapshot,
/// and publish it.
async fn full_scan(tx: &mpsc::Sender<ScanMsg>, latest: &mut Vec<models::SessionInfo>) {
    let sessions = tokio::task::spawn_blocking(scanner::scan_sessions)
        .await
        .unwrap_or_default();
    *latest = sessions.clone();
    let _ = tx.send(ScanMsg::SessionList(sessions)).await;
}

/// Refresh per-task usage stats every 30s.
pub(crate) fn spawn_task_stats(task_stats_tx: mpsc::Sender<ScanMsg>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Ok(stats) = tokio::task::spawn_blocking(hal_lib::tasks::stats::refresh).await {
                if task_stats_tx.send(ScanMsg::TaskStats(stats)).await.is_err() {
                    break;
                }
            }
        }
    });
}

/// Run one metrics scan, streaming progress as it goes.
pub(crate) fn spawn_metrics(tx: mpsc::Sender<ScanMsg>) {
    tokio::spawn(async move {
        let progress_tx = tx.clone();
        let fut = tokio::task::spawn_blocking(move || {
            // Throttle progress updates: the scanner rips through
            // several hundred files per second on warm cache, so report
            // at most every ~20 files (plus the 0 and N boundaries) to
            // keep the 16-slot channel from ever filling.
            let mut last_sent: usize = 0;
            metrics::analyze_with_progress(|scanned, total| {
                let at_edge = scanned == 0 || scanned == total;
                if at_edge || scanned.saturating_sub(last_sent) >= 20 {
                    last_sent = scanned;
                    let _ = progress_tx.try_send(ScanMsg::MetricsProgress { scanned, total });
                }
            })
        });
        if let Ok(m) = fut.await {
            let _ = tx.send(ScanMsg::Metrics(m)).await;
        }
    });
}

/// Deliver `prompt` (see [`send::Delivery`]) off the event-loop thread and
/// report the outcome over `tx` as a [`ScanMsg::DispatchResult`]. `send_prompt` forks+execs tmux
/// twice and sleeps ~80ms, which would freeze render and input for
/// 100-160ms. The status line is `ok_msg` on success and
/// `"<err_prefix>: <error>"` on failure, which is also logged.
pub(crate) fn spawn_dispatch(
    tx: mpsc::Sender<ScanMsg>,
    tmux: String,
    prompt: String,
    delivery: send::Delivery,
    ok_msg: String,
    err_prefix: String,
) {
    tokio::spawn(async move {
        let ok = tokio::task::spawn_blocking(move || delivery.deliver(&tmux, &prompt))
            .await
            .unwrap_or_else(|e| Err(io::Error::other(format!("dispatch task panicked: {}", e))))
            .map(|()| ok_msg)
            .map_err(|e| {
                log::warn!("dispatch: send_prompt failed: {}", e);
                format!("{}: {}", err_prefix, e)
            });
        let _ = tx.send(ScanMsg::DispatchResult { ok }).await;
    });
}
