//! User-intent commands and their side-effect contract.
//!
//! `bin/` maps key events onto [`Command`]s; [`App::execute`] performs every
//! in-process consequence — App state mutation, board writes, anything behind
//! [`crate::agent_runtime::AgentRuntime`] — and returns the [`Effect`]s that
//! need bin-side machinery: the terminal (pane sizes), tokio channels, or OS
//! window management. That split keeps the whole decision layer testable
//! without a terminal: tests execute commands against a recording runtime and
//! assert on state plus returned effects.
//!
//! Sessions- and Tasks-view commands are ported; Projects/Metrics arms still
//! live in `bin/src/keys.rs`. Modal buffer-edit keys (task input/tags/filter
//! character editing) stay as thin `bin` arms — only their submit/logic arms
//! became commands.

mod builds;
mod harness;
mod harness_settings;
mod sessions;
mod tasks;

pub use builds::BuildsCommand;
pub use harness::HarnessCommand;
pub use sessions::SessionsCommand;
pub use tasks::TasksCommand;

use super::{App, Tab};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Global(GlobalCommand),
    Sessions(SessionsCommand),
    Tasks(TasksCommand),
    Harness(HarnessCommand),
    Builds(BuildsCommand),
}

/// Commands available on any tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalCommand {
    Quit,
    /// Tab / Shift+K forward, BackTab / Shift+J backward.
    CycleTab {
        back: bool,
    },
    /// `m` on the Sessions tab.
    SetTabMetrics,
}

/// Side effects [`App::execute`] cannot perform in-process: they need the
/// terminal, a tokio channel owned by `run()`, or the window manager. The
/// bin-side interpreter (`bin/src/effects.rs`) owns exactly these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Fetch the session-detail payload for the popup (detail channel).
    RequestSessionDetail { session_id: String },
    /// Kick the background metrics analysis.
    SpawnMetricsScan,
    /// Walk the transcript stores off the event loop and deliver the archive
    /// to the session finder ([`crate::sessions::index::scan`]).
    BuildSessionIndex,
    /// Attach a tmux session as the embedded pane. `owned` panes are killed
    /// with the pane (shell panes); un-owned panes outlive it (agents).
    OpenTmuxPane { tmux: String, owned: bool },
    /// Spawn a shell tmux session in `cwd`, then attach it as an owned pane.
    OpenShell { cwd: String },
    /// Focus the OS window hosting `pid`, falling back to a tmux reattach
    /// in `cwd` when the window manager reports the session is detached.
    FocusWindow { pid: u32, cwd: String },
    /// Open a URL or path with the OS opener.
    OpenExternal { target: String },
}

impl App {
    /// Execute a user command: apply every in-process consequence and return
    /// the effects bin must interpret. Status messaging happens here so the
    /// interpreter stays a thin IO shim.
    pub fn execute(&mut self, cmd: Command) -> Vec<Effect> {
        match cmd {
            Command::Global(c) => self.execute_global(c),
            Command::Sessions(c) => self.execute_sessions(c),
            Command::Tasks(c) => self.execute_tasks(c),
            Command::Harness(c) => self.execute_harness(c),
            Command::Builds(c) => self.execute_builds(c),
        }
    }

    fn execute_global(&mut self, cmd: GlobalCommand) -> Vec<Effect> {
        match cmd {
            GlobalCommand::Quit => {
                self.should_quit = true;
                Vec::new()
            }
            GlobalCommand::CycleTab { back } => {
                let was_metrics = self.current_tab == Tab::Metrics;
                if back {
                    self.cycle_tab_back();
                } else {
                    self.cycle_tab();
                }
                self.metrics_scan_effect(was_metrics)
            }
            GlobalCommand::SetTabMetrics => {
                let was_metrics = self.current_tab == Tab::Metrics;
                self.set_tab(Tab::Metrics);
                self.metrics_scan_effect(was_metrics)
            }
        }
    }

    /// The metrics tab computes lazily: landing on it without an analysis
    /// kicks the scan exactly once.
    fn metrics_scan_effect(&self, was_metrics: bool) -> Vec<Effect> {
        if !was_metrics && self.current_tab == Tab::Metrics && self.metrics.analysis.is_none() {
            vec![Effect::SpawnMetricsScan]
        } else {
            Vec::new()
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::test_support::app_with;

    #[test]
    fn cycle_tab_into_metrics_requests_scan_once() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = app_with(vec![]);
            // Cycle until we land on Metrics; expect the scan effect there.
            let mut saw_scan = false;
            for _ in 0..4 {
                let effects = app.execute(Command::Global(GlobalCommand::CycleTab { back: false }));
                if app.current_tab == Tab::Metrics {
                    assert_eq!(effects, vec![Effect::SpawnMetricsScan]);
                    saw_scan = true;
                    break;
                }
                assert!(effects.is_empty());
            }
            assert!(saw_scan, "never landed on Metrics tab");
        });
    }

    #[test]
    fn quit_sets_flag() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = app_with(vec![]);
            app.execute(Command::Global(GlobalCommand::Quit));
            assert!(app.should_quit);
        });
    }
}
