//! Simple directory browser used when spawning a new tmux session from a
//! user-chosen path. Shows the subdirectories of `current_dir` and lets the
//! caller descend, ascend, or pick the current dir itself.
//!
//! Also supports two flat modes where descend/ascend are no-ops and picking
//! just selects the entry: [`PickerMode::Bookmarks`] over `entries`
//! (absolute paths from [`crate::bookmarks::Bookmarks`]), and
//! [`PickerMode::Places`] over `places` — a labelled candidate list
//! (registered projects, bookmarks, recent cwds) narrowed live by a fuzzy
//! `filter` the user types into.

use crate::fuzzy;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerMode {
    Browse,
    Bookmarks,
    Places,
}

/// Where a [`Place`] candidate came from, for the badge in the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceSource {
    Project,
    Bookmark,
    Recent,
}

/// One candidate row for [`PickerMode::Places`]. `name` and `display_path`
/// are precomputed at construction because the fuzzy filter matches against
/// exactly what is rendered — matching the raw path while displaying a
/// `~`-abbreviated one would misalign the highlight indices.
#[derive(Clone, Debug)]
pub struct Place {
    /// Project name when registered, else the path's basename.
    pub name: String,
    /// `~`-abbreviated path, shown dimmed next to the name.
    pub display_path: String,
    /// Absolute path handed to the pick handler.
    pub path: PathBuf,
    pub source: PlaceSource,
}

impl Place {
    pub fn new(label: Option<String>, path: PathBuf, source: PlaceSource) -> Self {
        let name = label.unwrap_or_else(|| {
            path.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        });
        let display_path = abbrev_home(&path);
        Self {
            name,
            display_path,
            path,
            source,
        }
    }
}

/// One visible row in Places mode: an index into `places` plus the char
/// indices the filter matched, for highlighting. At most one of the two
/// index sets is non-empty — whichever of name/path won the score.
#[derive(Clone, Debug, Default)]
pub struct PlaceRow {
    pub place: usize,
    pub name_indices: Vec<usize>,
    pub path_indices: Vec<usize>,
}

pub struct FolderPicker {
    pub current_dir: PathBuf,
    pub entries: Vec<String>,
    pub selection: usize,
    pub mode: PickerMode,
    /// Candidate list for [`PickerMode::Places`]; empty in other modes.
    pub places: Vec<Place>,
    /// Fuzzy query typed in Places mode.
    pub filter: String,
    /// Visible Places rows, best match first (candidate order when the
    /// filter is empty).
    pub rows: Vec<PlaceRow>,
}

impl FolderPicker {
    pub fn new(start: PathBuf) -> Self {
        let mut picker = Self {
            current_dir: start,
            entries: Vec::new(),
            selection: 0,
            mode: PickerMode::Browse,
            places: Vec::new(),
            filter: String::new(),
            rows: Vec::new(),
        };
        picker.reload();
        picker
    }

    /// Construct a picker pre-populated with `bookmarks` as a flat list.
    /// `current_dir` is set to the deepest common ancestor of the entries
    /// (or `/` when empty) just so legacy callers that read it have
    /// something sensible — it has no effect on rendering in this mode.
    pub fn new_bookmarks(bookmarks: Vec<PathBuf>) -> Self {
        let current_dir = bookmarks
            .first()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/"));
        let entries = bookmarks
            .into_iter()
            .map(|p| p.display().to_string())
            .collect();
        Self {
            current_dir,
            entries,
            selection: 0,
            mode: PickerMode::Bookmarks,
            places: Vec::new(),
            filter: String::new(),
            rows: Vec::new(),
        }
    }

    /// Construct a picker over a flat candidate list with an empty filter.
    pub fn new_places(places: Vec<Place>) -> Self {
        let mut picker = Self {
            current_dir: PathBuf::from("/"),
            entries: Vec::new(),
            selection: 0,
            mode: PickerMode::Places,
            places,
            filter: String::new(),
            rows: Vec::new(),
        };
        picker.refilter();
        picker
    }

    pub fn reload(&mut self) {
        if self.mode != PickerMode::Browse {
            // Flat-mode entries are authoritative; reload would wipe them.
            return;
        }
        self.entries = list_subdirs(&self.current_dir);
        self.selection = 0;
    }

    pub fn push_filter(&mut self, c: char) {
        self.filter.push(c);
        self.refilter();
    }

    pub fn pop_filter(&mut self) {
        self.filter.pop();
        self.refilter();
    }

    /// Recompute `rows` from `filter`. A name match counts double so the
    /// thing the user most likely typed (a project name) outranks an
    /// incidental hit somewhere in another candidate's path; the highlight
    /// goes to whichever side won. Resets the cursor — the old selection
    /// points at a different row set.
    fn refilter(&mut self) {
        if self.filter.is_empty() {
            self.rows = (0..self.places.len())
                .map(|place| PlaceRow {
                    place,
                    ..Default::default()
                })
                .collect();
        } else {
            let mut scored: Vec<(i32, PlaceRow)> = Vec::new();
            for (i, place) in self.places.iter().enumerate() {
                let name_m = fuzzy::fuzzy_match(&self.filter, &place.name);
                let path_m = fuzzy::fuzzy_match(&self.filter, &place.display_path);
                let name_score = name_m.as_ref().map(|m| m.score * 2);
                let path_score = path_m.as_ref().map(|m| m.score);
                let Some(score) = name_score.max(path_score) else {
                    continue;
                };
                let row = if name_score >= path_score {
                    PlaceRow {
                        place: i,
                        name_indices: name_m.map(|m| m.indices).unwrap_or_default(),
                        path_indices: Vec::new(),
                    }
                } else {
                    PlaceRow {
                        place: i,
                        name_indices: Vec::new(),
                        path_indices: path_m.map(|m| m.indices).unwrap_or_default(),
                    }
                };
                scored.push((score, row));
            }
            // Stable by candidate order on ties, so equal-score rows keep
            // the projects → bookmarks → recents precedence.
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.place.cmp(&b.1.place)));
            self.rows = scored.into_iter().map(|(_, r)| r).collect();
        }
        self.selection = 0;
    }

    /// Rows/entries currently navigable, depending on mode.
    pub fn visible_len(&self) -> usize {
        match self.mode {
            PickerMode::Places => self.rows.len(),
            _ => self.entries.len(),
        }
    }

    /// Move the cursor to the row whose place is `path`, if visible.
    /// Used to pre-select a task's previous cwd on re-assign.
    pub fn select_path(&mut self, path: &Path) {
        if let Some(idx) = self
            .rows
            .iter()
            .position(|r| self.places.get(r.place).is_some_and(|p| p.path == path))
        {
            self.selection = idx;
        }
    }

    pub fn selected_place(&self) -> Option<&Place> {
        self.rows
            .get(self.selection)
            .and_then(|r| self.places.get(r.place))
    }

    pub fn move_up(&mut self) {
        if self.selection > 0 {
            self.selection -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selection + 1 < self.visible_len() {
            self.selection += 1;
        }
    }

    pub fn descend(&mut self) {
        if self.mode != PickerMode::Browse {
            return;
        }
        let Some(name) = self.entries.get(self.selection) else {
            return;
        };
        self.current_dir = self.current_dir.join(name);
        self.reload();
    }

    pub fn ascend(&mut self) {
        if self.mode != PickerMode::Browse {
            return;
        }
        let Some(parent) = self.current_dir.parent().map(Path::to_path_buf) else {
            return;
        };
        let prior = self
            .current_dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned());
        self.current_dir = parent;
        self.reload();
        if let Some(name) = prior {
            if let Some(idx) = self.entries.iter().position(|e| e == &name) {
                self.selection = idx;
            }
        }
    }

    /// Absolute path of the currently-highlighted entry. In Browse mode
    /// that's `current_dir/entry`; in Bookmarks mode the entry already is
    /// an absolute path; in Places mode it's the highlighted candidate's
    /// path. Returns `None` when the (filtered) list is empty.
    pub fn selected_path(&self) -> Option<PathBuf> {
        if self.mode == PickerMode::Places {
            return self.selected_place().map(|p| p.path.clone());
        }
        let entry = self.entries.get(self.selection)?;
        Some(match self.mode {
            PickerMode::Browse => self.current_dir.join(entry),
            PickerMode::Bookmarks => PathBuf::from(entry),
            PickerMode::Places => unreachable!("handled above"),
        })
    }

    /// Remove the highlighted entry from `entries` and clamp the cursor.
    /// Only meaningful in Bookmarks mode — keeps the view in sync with the
    /// backing store after a toggle-off without rebuilding the picker.
    pub fn remove_selected(&mut self) {
        if self.selection >= self.entries.len() {
            return;
        }
        self.entries.remove(self.selection);
        if self.selection >= self.entries.len() && self.selection > 0 {
            self.selection -= 1;
        }
    }
}

fn list_subdirs(path: &Path) -> Vec<String> {
    let Ok(read) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut out: Vec<String> = read
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    out.sort_by_key(|s| s.to_lowercase());
    out
}

/// `$HOME`-prefixed paths render as `~/…`, everything else verbatim.
fn abbrev_home(path: &Path) -> String {
    let s = path.display().to_string();
    if let Some(home) = dirs::home_dir() {
        let h = home.display().to_string();
        if let Some(rest) = s.strip_prefix(&h) {
            if rest.is_empty() {
                return "~".to_string();
            }
            if rest.starts_with('/') {
                return format!("~{}", rest);
            }
        }
    }
    s
}

// Unix-only for the same reason as the other `$HOME`-redirecting tests:
// `dirs::home_dir()` ignores `$HOME` on Windows.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_util::with_temp_home;

    fn places() -> Vec<Place> {
        vec![
            Place::new(
                Some("cc-hub".into()),
                PathBuf::from("/g/self/cc-hub"),
                PlaceSource::Project,
            ),
            Place::new(None, PathBuf::from("/g/self/reddit"), PlaceSource::Bookmark),
            Place::new(
                None,
                PathBuf::from("/g/work/hub-tools"),
                PlaceSource::Recent,
            ),
        ]
    }

    #[test]
    fn empty_filter_shows_all_in_candidate_order() {
        with_temp_home(|| {
            let p = FolderPicker::new_places(places());
            assert_eq!(p.mode, PickerMode::Places);
            assert_eq!(
                p.rows.iter().map(|r| r.place).collect::<Vec<_>>(),
                vec![0, 1, 2]
            );
            assert_eq!(p.selected_path(), Some(PathBuf::from("/g/self/cc-hub")));
        });
    }

    #[test]
    fn filter_narrows_and_highlights_name_matches() {
        with_temp_home(|| {
            let mut p = FolderPicker::new_places(places());
            for c in "hub".chars() {
                p.push_filter(c);
            }
            // "reddit" has no h/u/b anywhere in name or path — dropped.
            // hub-tools starts with the query, which outranks cc-hub's
            // mid-name boundary match.
            let visible: Vec<usize> = p.rows.iter().map(|r| r.place).collect();
            assert_eq!(visible, vec![2, 0]);
            assert_eq!(p.selected_place().unwrap().name, "hub-tools");
            assert_eq!(p.rows[0].name_indices, vec![0, 1, 2]);
            assert_eq!(p.rows[1].name_indices, vec![3, 4, 5]);
        });
    }

    #[test]
    fn name_match_outranks_path_only_match() {
        with_temp_home(|| {
            let mut p = FolderPicker::new_places(vec![
                Place::new(
                    None,
                    PathBuf::from("/g/alpha-archive/beta"),
                    PlaceSource::Recent,
                ),
                Place::new(None, PathBuf::from("/g/x/alpha"), PlaceSource::Recent),
            ]);
            for c in "alpha".chars() {
                p.push_filter(c);
            }
            // The place *named* alpha wins over the one that only carries
            // alpha in its path, despite candidate order; the loser is
            // highlighted on the path side.
            assert_eq!(p.selected_place().unwrap().name, "alpha");
            assert!(p.rows[1].name_indices.is_empty());
            assert!(!p.rows[1].path_indices.is_empty());
        });
    }

    #[test]
    fn pop_filter_restores_and_no_match_empties() {
        with_temp_home(|| {
            let mut p = FolderPicker::new_places(places());
            for c in "zzz".chars() {
                p.push_filter(c);
            }
            assert!(p.rows.is_empty());
            assert_eq!(p.selected_path(), None);
            p.pop_filter();
            p.pop_filter();
            p.pop_filter();
            assert_eq!(p.rows.len(), 3);
        });
    }

    #[test]
    fn select_path_moves_cursor_and_flat_mode_navigation_clamps() {
        with_temp_home(|| {
            let mut p = FolderPicker::new_places(places());
            p.select_path(Path::new("/g/work/hub-tools"));
            assert_eq!(p.selection, 2);
            p.move_down();
            assert_eq!(p.selection, 2);
            // descend/ascend are no-ops in flat mode.
            p.descend();
            p.ascend();
            assert_eq!(p.selected_path(), Some(PathBuf::from("/g/work/hub-tools")));
        });
    }

    #[test]
    fn place_name_falls_back_to_basename() {
        with_temp_home(|| {
            let p = Place::new(None, PathBuf::from("/g/self/reddit"), PlaceSource::Recent);
            assert_eq!(p.name, "reddit");
        });
    }

    #[test]
    fn display_path_abbreviates_home() {
        with_temp_home(|| {
            let home = dirs::home_dir().unwrap();
            let p = Place::new(None, home.join("git/x"), PlaceSource::Recent);
            assert_eq!(p.display_path, "~/git/x");
            let outside = Place::new(None, PathBuf::from("/srv/y"), PlaceSource::Recent);
            assert_eq!(outside.display_path, "/srv/y");
        });
    }
}
