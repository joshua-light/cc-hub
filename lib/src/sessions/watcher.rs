use crate::platform::paths;
use log::{debug, warn};
use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebounceEventResult};
use std::time::Duration;
use tokio::sync::mpsc;

const DEBOUNCE: Duration = Duration::from_millis(100);

/// Watch every agent home and signal `tx` once per debounced batch of
/// changes. The channel is expected to hold one slot: a full channel means a
/// rescan is already pending, so the signal is dropped rather than queued.
pub fn spawn_fs_watcher(tx: mpsc::Sender<()>) {
    let claude = paths::claude_home();
    let pi = paths::pi_home();
    let codex = paths::codex_home();
    let cc_hub = paths::cc_hub_home();
    if claude.is_none() && pi.is_none() && codex.is_none() && cc_hub.is_none() {
        warn!("fs watcher: no agent homes resolvable, skipping");
        return;
    }

    std::thread::spawn(move || {
        let (std_tx, std_rx) = std::sync::mpsc::channel::<DebounceEventResult>();
        let mut debouncer = match new_debouncer(DEBOUNCE, std_tx) {
            Ok(d) => d,
            Err(e) => {
                warn!("fs watcher: failed to create debouncer: {}", e);
                return;
            }
        };

        let watcher = debouncer.watcher();
        let mut targets = Vec::new();
        if let Some(claude) = claude {
            targets.push((claude.join("sessions"), RecursiveMode::Recursive));
            targets.push((claude.join("projects"), RecursiveMode::Recursive));
            targets.push((claude.join("history.jsonl"), RecursiveMode::NonRecursive));
        }
        if let Some(pi) = pi {
            targets.push((pi.join("sessions"), RecursiveMode::Recursive));
        }
        if let Some(codex) = codex {
            targets.push((codex.join("sessions"), RecursiveMode::Recursive));
        }
        if let Some(cc_hub) = cc_hub {
            targets.push((cc_hub.join("pi-heartbeats"), RecursiveMode::Recursive));
        }

        for account in crate::resources::accounts().values() {
            if let Some(home) = account.home() {
                targets.push((home.join("sessions"), RecursiveMode::Recursive));
                if account.provider == crate::agent::AgentKind::Claude {
                    targets.push((home.join("projects"), RecursiveMode::Recursive));
                    targets.push((home.join("history.jsonl"), RecursiveMode::NonRecursive));
                }
            }
        }
        for (path, mode) in &targets {
            match watcher.watch(path, *mode) {
                Ok(()) => debug!("fs watcher: watching {}", path.display()),
                Err(e) => warn!("fs watcher: cannot watch {}: {}", path.display(), e),
            }
        }

        while let Ok(res) = std_rx.recv() {
            match res {
                Ok(events) => debug!("fs watcher: {} debounced event(s)", events.len()),
                Err(e) => {
                    debug!("fs watcher: notify error: {:?}", e);
                    continue;
                }
            }
            if let Err(mpsc::error::TrySendError::Closed(())) = tx.try_send(()) {
                break;
            }
        }
    });
}
