//! The folder picker in its three modes (known places, filesystem browse,
//! bookmarks), shared by the Tasks assign flow and the Sessions spawn flows,
//! plus the GitHub-repo overlay drawn on top of it.

use super::{App, View};
use crate::folder_picker::{FolderPicker, PickerMode, Place};
use std::collections::HashSet;
use std::path::PathBuf;

/// Overlay on top of [`View::FolderPicker`] that prompts for a new GitHub
/// repo name. `cwd` is captured at open time so the run target can't drift
/// if the picker is reloaded while the input is active.
#[derive(Clone, Debug)]
pub struct GhCreateInput {
    pub name: String,
    pub private: bool,
    pub cwd: String,
}

impl App {
    /// Candidates for the task-assign places picker: bookmarks first, then
    /// recently-used directories (other board tasks, scanned sessions)
    /// newest-first — deduped by path. This order is what an empty filter
    /// shows.
    pub fn known_places(&self) -> Vec<Place> {
        use crate::folder_picker::PlaceSource;
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut out: Vec<Place> = Vec::new();
        for path in self.bookmarks.list() {
            if seen.insert(path.clone()) {
                out.push(Place::new(path, PlaceSource::Bookmark));
            }
        }
        // Recents carry a coarse timestamp purely for ordering: a task's
        // creation (or completion) time, a session's last activity.
        let mut recents: Vec<(u64, PathBuf)> = Vec::new();
        for t in self.tasks.board.tasks() {
            if let Some(cwd) = t.cwd.as_deref() {
                recents.push((
                    t.created_at.max(t.done_at.unwrap_or(0)).max(0) as u64,
                    PathBuf::from(cwd),
                ));
            }
        }
        for s in &self.sessions.last_sessions {
            recents.push((
                s.last_activity.unwrap_or(s.started_at),
                PathBuf::from(&s.cwd),
            ));
        }
        recents.sort_by_key(|(ts, _)| std::cmp::Reverse(*ts));
        const MAX_RECENTS: usize = 15;
        let mut added = 0usize;
        for (_, path) in recents {
            if added >= MAX_RECENTS {
                break;
            }
            if seen.insert(path.clone()) {
                out.push(Place::new(path, PlaceSource::Recent));
                added += 1;
            }
        }
        out
    }

    /// Places for the assign picker: [`Self::known_places`] with the most
    /// recently assigned cwd promoted to the front (and thus selected by
    /// default), so firing several tasks at one project is a plain Enter
    /// each time. A last cwd no longer among the known places is
    /// resurrected as a Recent entry.
    pub(super) fn assign_places(&self) -> Vec<Place> {
        use crate::folder_picker::PlaceSource;
        let mut places = self.known_places();
        if let Some(last) = self.tasks.board.last_assign_cwd() {
            let last = std::path::Path::new(last);
            // `S` quick-assigns at $HOME; that's not a project choice
            // worth promoting (or resurrecting) here.
            if Some(last) == dirs::home_dir().as_deref() {
                return places;
            }
            if let Some(idx) = places.iter().position(|p| p.path == last) {
                let place = places.remove(idx);
                places.insert(0, place);
            } else {
                places.insert(0, Place::new(last.to_path_buf(), PlaceSource::Recent));
            }
        }
        places
    }

    /// Tab in the places/browse picker: flip between the known-places list
    /// and the filesystem browser. Serves both the task-assign flow and the
    /// Sessions-tab `N` flow; no-op when there is nothing to flip to.
    pub fn toggle_places_picker_mode(&mut self) {
        let Some(picker) = self.folder_picker.as_ref() else {
            return;
        };
        let assigning = self.tasks.pending_assign.clone();
        match picker.mode {
            PickerMode::Places => {
                let prev_cwd = match &assigning {
                    Some(id) => self.tasks.board.get(id).and_then(|t| t.cwd.clone()),
                    None => self.selected_session_info().map(|s| s.cwd.clone()),
                };
                self.folder_picker = Some(FolderPicker::new(Self::assign_browse_start(
                    prev_cwd.as_deref(),
                )));
            }
            PickerMode::Browse => {
                let places = if assigning.is_some() {
                    self.assign_places()
                } else {
                    self.known_places()
                };
                if !places.is_empty() {
                    self.folder_picker = Some(FolderPicker::new_places(places));
                }
            }
            PickerMode::Bookmarks => {}
        }
    }

    /// Browse-mode starting point for an assignment: the task's previous
    /// cwd, else `$HOME`, else `/`.
    pub(super) fn assign_browse_start(prev_cwd: Option<&str>) -> PathBuf {
        prev_cwd
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    pub fn enter_folder_picker(&mut self) {
        let start = self
            .selected_session_info()
            .map(|s| PathBuf::from(&s.cwd))
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        self.folder_picker = Some(FolderPicker::new(start));
        self.view = View::FolderPicker;
    }

    /// `p` on the Sessions tab: the task-assign places picker, choosing
    /// where the new session spawns. Falls back to the filesystem browser
    /// when nothing is known yet. The selected session's cwd starts
    /// highlighted.
    pub fn enter_session_places_picker(&mut self) {
        let places = self.known_places();
        if places.is_empty() {
            self.enter_folder_picker();
            return;
        }
        let mut picker = FolderPicker::new_places(places);
        if let Some(cwd) = self.selected_session_info().map(|s| s.cwd.clone()) {
            picker.select_path(std::path::Path::new(&cwd));
        }
        self.folder_picker = Some(picker);
        self.view = View::FolderPicker;
    }

    /// Open the picker pre-loaded with the user's bookmarked folders.
    /// Returns `false` (no-op) when no bookmarks exist so the caller can
    /// show a hint instead of silently opening an empty popup.
    pub fn enter_bookmarks_picker(&mut self) -> bool {
        let entries = self.bookmarks.list();
        if entries.is_empty() {
            return false;
        }
        self.folder_picker = Some(FolderPicker::new_bookmarks(entries));
        self.view = View::FolderPicker;
        true
    }

    /// Toggle the bookmark on the highlighted picker entry. Returns the
    /// new state plus a display path the caller can use for status text,
    /// or `None` when no entry is selected. Also keeps the picker view
    /// in sync: a toggle-off in Bookmarks mode removes the row so the
    /// list doesn't show stale entries.
    pub fn toggle_selected_bookmark(&mut self) -> Option<(bool, String)> {
        let picker = self.folder_picker.as_mut()?;
        let path = picker.selected_path()?;
        let display = path.display().to_string();
        let added = self.bookmarks.toggle(path);
        if !added && picker.mode == PickerMode::Bookmarks {
            picker.remove_selected();
        }
        Some((added, display))
    }

    /// Best-guess cwd to spawn a new agent in: the selected session's cwd, or
    /// the user's home directory.
    pub fn default_spawn_cwd(&self) -> Option<String> {
        self.selected_session_info()
            .map(|s| s.cwd.clone())
            .or_else(|| dirs::home_dir().map(|p| p.display().to_string()))
    }

    pub fn close_folder_picker(&mut self) {
        self.folder_picker = None;
        self.gh_create_input = None;
        self.tasks.pending_assign = None;
        self.view = View::Grid;
    }

    /// Open the "create GitHub repo" overlay rooted in the picker's current
    /// directory. Prefills the repo name with the basename.
    pub fn enter_gh_create_input(&mut self, private: bool) {
        let Some(picker) = self.folder_picker.as_ref() else {
            return;
        };
        let cwd = picker.current_dir.display().to_string();
        let name = picker
            .current_dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.gh_create_input = Some(GhCreateInput { name, private, cwd });
        self.view = View::GhCreateInput;
    }

    pub fn close_gh_create_input(&mut self) {
        self.gh_create_input = None;
        self.leave_gh_create_input();
    }

    pub fn submit_gh_create_input(&mut self) -> Option<(String, String, bool)> {
        let input = self.gh_create_input.take()?;
        self.leave_gh_create_input();
        Some((input.cwd, input.name, input.private))
    }

    /// Back to the picker the overlay sat on, or the grid if it is gone.
    fn leave_gh_create_input(&mut self) {
        self.view = if self.folder_picker.is_some() {
            View::FolderPicker
        } else {
            View::Grid
        };
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::folder_picker::PlaceSource;
    use crate::tasks::store::TaskStatus;
    use crate::test_util::with_temp_home;

    #[test]
    fn known_places_orders_bookmarks_recents_and_dedups() {
        with_temp_home(|| {
            let mut app = App::new();
            app.bookmarks.toggle(PathBuf::from("/tmp/bm"));
            let id = app.tasks.board.add("t").unwrap().unwrap();
            app.tasks
                .board
                .assign(&id, "/tmp/recent", "claude", "mux-1")
                .unwrap();
            // A recent duplicating a bookmark must be swallowed.
            let dup = app.tasks.board.add("t2").unwrap().unwrap();
            app.tasks
                .board
                .assign(&dup, "/tmp/bm", "claude", "mux-2")
                .unwrap();

            let places = app.known_places();
            let got: Vec<(&str, PlaceSource)> =
                places.iter().map(|p| (p.name.as_str(), p.source)).collect();
            assert_eq!(
                got,
                vec![
                    ("bm", PlaceSource::Bookmark),
                    ("recent", PlaceSource::Recent),
                ]
            );
        });
    }

    #[test]
    fn assign_picker_opens_places_and_preselects_previous_cwd() {
        with_temp_home(|| {
            let mut app = App::new();
            app.bookmarks.toggle(PathBuf::from("/tmp/p-a"));
            app.bookmarks.toggle(PathBuf::from("/tmp/p-b"));
            let id = app.tasks.board.add("do the thing").unwrap().unwrap();
            // A previous assignment on p-b: reopening the picker must
            // land the cursor there, not on the first candidate.
            app.tasks
                .board
                .assign(&id, "/tmp/p-b", "claude", "mux-1")
                .unwrap();
            app.focus_task(&id);

            assert!(app.enter_task_assign_picker());
            let picker = app.folder_picker.as_ref().unwrap();
            assert_eq!(picker.mode, PickerMode::Places);
            assert_eq!(picker.selected_place().unwrap().name, "p-b");
            assert_eq!(app.tasks.pending_assign.as_deref(), Some(id.as_str()));
        });
    }

    #[test]
    fn assign_picker_promotes_last_assigned_project_to_front() {
        with_temp_home(|| {
            let mut app = App::new();
            app.bookmarks.toggle(PathBuf::from("/tmp/p-a"));
            app.bookmarks.toggle(PathBuf::from("/tmp/p-b"));
            // An earlier task went to p-b: a fresh task's picker must
            // open with p-b first and selected, ready for plain Enter.
            let prev = app.tasks.board.add("earlier").unwrap().unwrap();
            app.tasks
                .board
                .assign(&prev, "/tmp/p-b", "claude", "mux-1")
                .unwrap();
            let id = app.tasks.board.add("next").unwrap().unwrap();
            app.focus_task(&id);

            assert!(app.enter_task_assign_picker());
            let picker = app.folder_picker.as_ref().unwrap();
            let names: Vec<&str> = picker
                .rows
                .iter()
                .map(|r| picker.places[r.place].name.as_str())
                .collect();
            assert_eq!(names, vec!["p-b", "p-a"]);
            assert_eq!(picker.selected_place().unwrap().name, "p-b");
        });
    }

    #[test]
    fn assign_picker_resurrects_last_cwd_missing_from_places() {
        with_temp_home(|| {
            let mut app = App::new();
            app.bookmarks.toggle(PathBuf::from("/tmp/p-a"));
            // The assigned task is deleted, so /tmp/gone is in no
            // bookmark/recent — it must still lead the list.
            let prev = app.tasks.board.add("earlier").unwrap().unwrap();
            app.tasks
                .board
                .assign(&prev, "/tmp/gone", "claude", "mux-1")
                .unwrap();
            app.tasks.board.remove(&prev).unwrap();
            let id = app.tasks.board.add("next").unwrap().unwrap();
            app.focus_task(&id);

            assert!(app.enter_task_assign_picker());
            let picker = app.folder_picker.as_ref().unwrap();
            let first = picker.selected_place().unwrap();
            assert_eq!(first.name, "gone");
            assert_eq!(first.source, PlaceSource::Recent);
        });
    }

    #[test]
    fn assign_picker_falls_back_to_browse_when_nothing_known() {
        with_temp_home(|| {
            let mut app = App::new();
            let id = app.tasks.board.add("t").unwrap().unwrap();
            app.focus_task(&id);
            assert!(app.enter_task_assign_picker());
            assert_eq!(app.folder_picker.as_ref().unwrap().mode, PickerMode::Browse);
        });
    }

    #[test]
    fn tab_toggles_between_places_and_browse_in_assign_and_session_flows() {
        with_temp_home(|| {
            let mut app = App::new();
            app.bookmarks.toggle(PathBuf::from("/tmp/p-a"));
            let id = app.tasks.board.add("t").unwrap().unwrap();
            app.focus_task(&id);
            assert!(app.enter_task_assign_picker());
            let mode = |app: &App| app.folder_picker.as_ref().unwrap().mode;
            assert_eq!(mode(&app), PickerMode::Places);
            app.toggle_places_picker_mode();
            assert_eq!(mode(&app), PickerMode::Browse);
            app.toggle_places_picker_mode();
            assert_eq!(mode(&app), PickerMode::Places);

            // The sessions-tab `N` flow toggles the same way.
            app.close_folder_picker();
            app.enter_session_places_picker();
            assert_eq!(mode(&app), PickerMode::Places);
            app.toggle_places_picker_mode();
            assert_eq!(mode(&app), PickerMode::Browse);
            app.toggle_places_picker_mode();
            assert_eq!(mode(&app), PickerMode::Places);
        });
    }

    #[test]
    fn session_places_picker_opens_places_without_pending_assign() {
        with_temp_home(|| {
            let mut app = App::new();
            app.bookmarks.toggle(PathBuf::from("/tmp/p-a"));
            app.enter_session_places_picker();
            let picker = app.folder_picker.as_ref().unwrap();
            assert_eq!(picker.mode, PickerMode::Places);
            assert!(app.tasks.pending_assign.is_none());
            assert_eq!(app.view, View::FolderPicker);
        });
    }

    #[test]
    fn session_places_picker_falls_back_to_browse_when_nothing_known() {
        with_temp_home(|| {
            let mut app = App::new();
            app.enter_session_places_picker();
            assert_eq!(app.folder_picker.as_ref().unwrap().mode, PickerMode::Browse);
        });
    }

    #[test]
    fn assign_places_does_not_promote_home_quick_assign() {
        with_temp_home(|| {
            let mut app = App::new();
            app.bookmarks.toggle(PathBuf::from("/tmp/p-a"));
            app.bookmarks.toggle(PathBuf::from("/tmp/p-b"));
            let home = dirs::home_dir().unwrap().display().to_string();
            let prev = app.tasks.board.add("broad question").unwrap().unwrap();
            app.tasks
                .board
                .assign(&prev, &home, "claude", "mux-1")
                .unwrap();
            let id = app.tasks.board.add("next").unwrap().unwrap();
            app.focus_task(&id);

            assert!(app.enter_task_assign_picker());
            let picker = app.folder_picker.as_ref().unwrap();
            // $HOME must neither lead the list nor appear resurrected.
            assert_eq!(picker.selected_place().unwrap().name, "p-a");
        });
    }

    #[test]
    fn quick_assign_at_home_requires_focused_undone_task() {
        with_temp_home(|| {
            let mut app = App::new();
            // No task focused: refuse rather than spawn.
            assert!(app.assign_selected_task_at_home().is_none());
            let id = app.tasks.board.add("t").unwrap().unwrap();
            app.tasks.board.set_status(&id, TaskStatus::Done).unwrap();
            app.focus_task(&id);
            assert!(app.assign_selected_task_at_home().is_none());
            assert!(app.tasks.pending_assign.is_none());
        });
    }
}
