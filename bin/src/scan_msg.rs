use crate::titles::Titles;
use cc_hub_lib::app::{self, App};
use cc_hub_lib::sessions::count;
use cc_hub_lib::{harness, metrics, models, ui, usage};

/// A background worker's result, drained by the event loop between frames.
pub(crate) enum ScanMsg {
    SessionList(Vec<models::SessionInfo>),
    /// The full transcript archive for the session finder, built off the
    /// event loop by [`Effect::BuildSessionIndex`](app::Effect::BuildSessionIndex).
    SessionIndex(Vec<cc_hub_lib::sessions::index::IndexedSession>),
    Detail(models::SessionDetail),
    Usage(usage::UsageInfo),
    SessionCounts(count::SessionCounts),
    Metrics(metrics::MetricsAnalysis),
    TaskStats(Vec<(String, cc_hub_lib::tasks::stats::TaskStats)>),
    MetricsProgress {
        scanned: usize,
        total: usize,
    },
    GhCreateDone {
        name: String,
        result: Result<String, String>,
    },
    /// Fresh on-disk snapshot of every persistent agent (Agents tab).
    Harness(Vec<harness::AgentSnapshot>),
    /// A persistent agent finished a tick; carries its status-bar line.
    HarnessTick(harness::supervisor::TickReport),
    /// Fresh on-disk snapshot of every build and hold (Builds tab).
    Builds(app::BuildsSnapshot),
    /// What each recipe's player runs and who holds each resource.
    BuildsProbe(app::BuildsProbe),
    /// Result of a `send::send_prompt` run off the event-loop thread (see
    /// [`spawn_dispatch`](crate::workers::spawn_dispatch)). `send_prompt`
    /// forks+execs tmux twice and sleeps ~80ms; running it inline froze
    /// render+input. `ok` carries the status line built from the prompt's
    /// success/failure templates.
    DispatchResult {
        ok: Result<String, String>,
    },
}

/// Apply one drained [`ScanMsg`] to `app`. Extracted verbatim from the
/// `run()` channel-drain loop; the `title`-tracking maps/gate are threaded
/// through so the `SessionList` arm can kick off missing-title subprocesses
/// exactly as it did inline.
///
/// Returns true when the message changed anything the renderer shows. The
/// periodic `SessionList` scan arm reports no-change for the
/// common all-idle tick so the caller can skip the repaint; every other
/// message exists only to mutate visible state, so they always return true.
pub(crate) fn apply_scan_msg(app: &mut App, msg: ScanMsg, titles: &Titles) -> bool {
    match msg {
        ScanMsg::SessionList(mut sessions) => {
            titles.queue_missing(&mut sessions);
            return app.update_sessions(sessions);
        }
        ScanMsg::TaskStats(stats) => app.tasks.board.update_stats(stats),
        ScanMsg::SessionIndex(index) => app.update_session_index(index),
        ScanMsg::Detail(detail) => app.update_detail(detail),
        ScanMsg::Usage(u) => {
            let line = ui::build_usage_line(&u);
            app.update_usage(u, line);
        }
        ScanMsg::SessionCounts(c) => {
            app.update_session_counts(c);
        }
        ScanMsg::Metrics(m) => {
            app.update_metrics(m);
        }
        ScanMsg::MetricsProgress { scanned, total } => {
            app.update_metrics_progress(scanned, total);
        }
        ScanMsg::GhCreateDone { name, result } => {
            if let Some(picker) = app.folder_picker.as_mut() {
                picker.reload();
                if result.is_ok() {
                    if let Some(idx) = picker.entries.iter().position(|e| e == &name) {
                        picker.selection = idx;
                    }
                }
            }
            let status = match result {
                Ok(url) if !url.is_empty() => {
                    format!("created {} — press space to spawn", url)
                }
                Ok(_) => format!("created {} — press space to spawn", name),
                Err(e) => format!("gh create failed: {}", e),
            };
            app.set_status(status);
        }
        ScanMsg::Harness(agents) => app.update_harness(agents),
        ScanMsg::Builds(snapshot) => app.update_builds(snapshot),
        ScanMsg::BuildsProbe(probe) => app.update_builds_probe(probe),
        ScanMsg::HarnessTick(report) => {
            if !report.ok {
                app.set_status(report.status);
            } else {
                log::info!("harness: {}", report.status);
            }
        }
        ScanMsg::DispatchResult { ok } => {
            // Success and failure both already hold the rendered
            // status line; either way it goes straight to the bar.
            app.set_status(ok.unwrap_or_else(|e| e));
        }
    }
    true
}
