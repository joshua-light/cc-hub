//! The dock: the split layout's right-hand side, the selected session's tmux
//! pane attached live. One attach client serves every session: following the
//! cursor moves it with `switch-client` instead of ending it and starting
//! another. It follows once the cursor rests, so running down ten rows
//! doesn't switch through ten sessions — each switch resizes that session's
//! window. Enter moves the keyboard into it ([`crate::app::View::Dock`]), F1
//! hands it back to the list.

use super::SessionsLayout;
use crate::app::{App, Tab, View};
use crate::models::SessionState;
use crate::tmux_pane::TmuxPaneView;
use std::time::{Duration, Instant};

/// How long the cursor rests on a session before the dock shows it: long
/// enough to skip the rows a held key runs past, short next to the switch
/// itself (~6ms).
const SETTLE: Duration = Duration::from_millis(60);

#[derive(Default)]
pub struct Dock {
    target: Option<Target>,
    /// The attach client. Between a new target and its switch it still
    /// shows the previous session, and stays hidden.
    pane: Option<TmuxPaneView>,
}

/// The tmux session the dock shows, and how far it got attaching to it.
struct Target {
    tmux: String,
    /// When the attach may happen; `None` once it was tried, so a failed or
    /// exited attach waits for the cursor to move instead of retrying.
    due: Option<Instant>,
    failure: Option<String>,
}

impl Dock {
    /// The pane, while it shows the target.
    pub fn pane(&self) -> Option<&TmuxPaneView> {
        self.pane.as_ref().filter(|_| self.shows_target())
    }

    pub fn pane_mut(&mut self) -> Option<&mut TmuxPaneView> {
        let shows = self.shows_target();
        self.pane.as_mut().filter(|_| shows)
    }

    fn shows_target(&self) -> bool {
        match (&self.target, &self.pane) {
            (Some(target), Some(pane)) => target.due.is_none() && target.tmux == pane.session_name,
            _ => false,
        }
    }

    /// Whether an attach is still on its way.
    pub fn attaching(&self) -> bool {
        self.target.as_ref().is_some_and(|t| t.due.is_some())
    }

    /// Why the last attach didn't take.
    pub fn failure(&self) -> Option<&str> {
        self.target.as_ref()?.failure.as_deref()
    }

    /// Point the dock at `tmux`, `None` for nothing to show. A new target
    /// hides the pane at once and moves it there after [`SETTLE`], attaching
    /// a client first when there is none; `size` is the `(rows, cols)` it
    /// attaches at, and the renderer resizes it to the real area on the next
    /// frame. Nothing to show lets the client go. Returns whether what the
    /// dock shows changed.
    fn follow(&mut self, tmux: Option<&str>, size: (u16, u16)) -> bool {
        if self.target.as_ref().map(|t| t.tmux.as_str()) != tmux {
            self.aim(tmux, Instant::now() + SETTLE);
            return true;
        }
        let exited = self.pane.as_ref().is_some_and(|p| p.is_exited());
        if exited {
            self.pane = None;
        }
        let Some(target) = self.target.as_mut() else {
            return exited;
        };
        let due = target.due.is_some_and(|due| Instant::now() >= due);
        if !due {
            return exited;
        }
        target.due = None;
        if let Some(pane) = self.pane.as_mut() {
            match pane.switch_to(&target.tmux) {
                Ok(()) => return true,
                Err(e) => {
                    log::debug!("dock: switch to {} failed, attaching: {}", target.tmux, e);
                    self.pane = None;
                }
            }
        }
        let (rows, cols) = size;
        match TmuxPaneView::spawn(&target.tmux, rows, cols) {
            Ok(pane) => self.pane = Some(pane),
            Err(e) => {
                log::warn!("dock: attach {} failed: {}", target.tmux, e);
                target.failure = Some(e.to_string());
            }
        }
        true
    }

    /// Aim at `tmux` and show it on the next [`Self::follow`], skipping the
    /// settle: the user asked for this pane now.
    fn hurry(&mut self, tmux: &str) {
        let shown = self.shows_target();
        match self.target.as_mut() {
            Some(t) if t.tmux == tmux => {
                if !shown {
                    t.due = Some(Instant::now());
                }
            }
            _ => self.aim(Some(tmux), Instant::now()),
        }
    }

    fn aim(&mut self, tmux: Option<&str>, due: Instant) {
        if tmux.is_none() {
            self.pane = None;
        }
        self.target = tmux.map(|tmux| Target {
            tmux: tmux.to_string(),
            due: Some(due),
            failure: None,
        });
    }
}

impl App {
    /// Keep the dock on the selected session while the split layout is on
    /// screen, and empty it everywhere else. Called on every loop pass; a
    /// focused dock whose pane is gone hands the keyboard back to the list.
    /// Returns whether the frame needs a repaint.
    pub fn follow_dock(&mut self) -> bool {
        let tmux = self.docked_tmux();
        let mut changed = self
            .sessions
            .dock
            .follow(tmux.as_deref(), self.render.dock_size);
        let dock = &self.sessions.dock;
        if self.view == View::Dock && dock.pane.is_none() && !dock.attaching() {
            self.view = View::Grid;
            changed = true;
        }
        changed
    }

    /// `f`/Enter in the split layout: type into the selected session's pane.
    /// Returns `false` when it has none to type into.
    pub(crate) fn focus_dock(&mut self) -> bool {
        let Some(tmux) = self.docked_tmux() else {
            return false;
        };
        self.sessions.dock.hurry(&tmux);
        self.view = View::Dock;
        true
    }

    /// F1 in the dock: the keyboard goes back to the list.
    pub fn leave_dock(&mut self) {
        self.view = View::Grid;
    }

    /// The tmux session the dock should show: the selected live session's,
    /// while the split layout is on screen.
    fn docked_tmux(&self) -> Option<String> {
        let docked =
            self.current_tab == Tab::Sessions && self.sessions.layout == SessionsLayout::Split;
        if !docked {
            return None;
        }
        self.selected_session_info()
            .filter(|s| s.state != SessionState::Inactive)
            .and_then(|s| s.tmux_session.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_target_waits_for_the_cursor_to_settle() {
        let mut dock = Dock::default();
        assert!(dock.follow(Some("cc-a"), (24, 80)));
        assert!(dock.attaching());
        // Still inside the settle window: nothing to attach yet.
        assert!(!dock.follow(Some("cc-a"), (24, 80)));
        assert!(dock.pane().is_none());
    }

    #[test]
    fn moving_off_a_session_drops_its_target() {
        let mut dock = Dock::default();
        dock.follow(Some("cc-a"), (24, 80));
        assert!(dock.follow(None, (24, 80)));
        assert!(!dock.attaching());
        assert!(!dock.follow(None, (24, 80)));
    }
}
