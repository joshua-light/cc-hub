use crate::platform::paths;
use log::{debug, warn};
use notify_debouncer_mini::{
    new_debouncer,
    notify::RecursiveMode,
    DebounceEventResult,
};
use std::time::Duration;
use tokio::sync::mpsc;

const DEBOUNCE: Duration = Duration::from_millis(100);

/// Spawn a filesystem watcher that signals `tx` whenever anything of interest
/// under `~/.claude/` changes. Runs on a dedicated thread for the process
/// lifetime; the debouncer is leaked into that thread so it's never dropped.
///
/// Watched paths (best-effort — failures to watch are logged, not fatal):
///   - `~/.claude/sessions/` — session metadata (new/resumed/ended sessions)
///   - `~/.claude/projects/` — JSONL conversation files (/clear chains live here)
///   - `~/.claude/history.jsonl` — user prompts, source of /clear events
///
/// Recursive watches cover project subdirs that appear after startup.
pub fn spawn_fs_watcher(tx: mpsc::Sender<()>) {
    let Some(claude) = paths::claude_home() else {
        warn!("fs watcher: ~/.claude unresolvable, skipping");
        return;
    };

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
        let targets = [
            (claude.join("sessions"), RecursiveMode::Recursive),
            (claude.join("projects"), RecursiveMode::Recursive),
            (claude.join("history.jsonl"), RecursiveMode::NonRecursive),
        ];
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
            if tx.blocking_send(()).is_err() {
                break;
            }
        }
    });
}
