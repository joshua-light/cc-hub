use crate::platform::paths;
use log::{debug, warn};
use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebounceEventResult};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;

const DEBOUNCE: Duration = Duration::from_millis(100);

/// A debounced filesystem batch classified by the snapshot it invalidates.
/// Keeping this information avoids coupling an agent transcript write to an
/// unrelated Projects scan (and vice versa).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WatchBatch {
    pub sessions: bool,
    pub projects: bool,
}

fn classify_paths<'a>(
    changed: impl IntoIterator<Item = &'a Path>,
    projects_root: Option<&Path>,
    projects_registry: Option<&Path>,
) -> WatchBatch {
    let mut batch = WatchBatch::default();
    for path in changed {
        let is_project = projects_root.is_some_and(|root| path.starts_with(root))
            || projects_registry.is_some_and(|registry| path == registry);
        batch.projects |= is_project;
        batch.sessions |= !is_project;
    }
    batch
}

pub fn spawn_fs_watcher(tx: mpsc::Sender<WatchBatch>) {
    let claude = paths::claude_home();
    let pi = paths::pi_home();
    let cc_hub = paths::cc_hub_home();
    if claude.is_none() && pi.is_none() && cc_hub.is_none() {
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
        if let Some(cc_hub) = cc_hub {
            targets.push((cc_hub.join("pi-heartbeats"), RecursiveMode::Recursive));
            targets.push((cc_hub.join("projects"), RecursiveMode::Recursive));
            targets.push((cc_hub.join("projects.toml"), RecursiveMode::NonRecursive));
        }

        for (path, mode) in &targets {
            match watcher.watch(path, *mode) {
                Ok(()) => debug!("fs watcher: watching {}", path.display()),
                Err(e) => warn!("fs watcher: cannot watch {}: {}", path.display(), e),
            }
        }

        while let Ok(res) = std_rx.recv() {
            let events = match res {
                Ok(events) => {
                    debug!("fs watcher: {} debounced event(s)", events.len());
                    events
                }
                Err(e) => {
                    debug!("fs watcher: notify error: {:?}", e);
                    continue;
                }
            };
            let cc_hub_projects = paths::cc_hub_home().map(|h| h.join("projects"));
            let cc_hub_registry = paths::cc_hub_home().map(|h| h.join("projects.toml"));
            let changed: Vec<PathBuf> = events.iter().map(|event| event.path.clone()).collect();
            let batch = classify_paths(
                changed.iter().map(PathBuf::as_path),
                cc_hub_projects.as_deref(),
                cc_hub_registry.as_deref(),
            );
            if tx.blocking_send(batch).is_err() {
                break;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_session_and_project_changes_independently() {
        let projects = Path::new("/home/u/.cc-hub/projects");
        let registry = Path::new("/home/u/.cc-hub/projects.toml");
        assert_eq!(
            classify_paths(
                [Path::new("/home/u/.claude/projects/p/s.jsonl")],
                Some(projects),
                Some(registry)
            ),
            WatchBatch {
                sessions: true,
                projects: false
            }
        );
        assert_eq!(
            classify_paths(
                [Path::new("/home/u/.cc-hub/projects/p/tasks/t/state.json")],
                Some(projects),
                Some(registry)
            ),
            WatchBatch {
                sessions: false,
                projects: true
            }
        );
        assert_eq!(
            classify_paths(
                [
                    Path::new("/home/u/.pi/sessions/s.jsonl"),
                    Path::new("/home/u/.cc-hub/projects.toml")
                ],
                Some(projects),
                Some(registry)
            ),
            WatchBatch {
                sessions: true,
                projects: true
            }
        );
    }
}
