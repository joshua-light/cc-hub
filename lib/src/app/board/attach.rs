//! The Task Info popup (`v`) and card attachments: notes, files and URLs.

use crate::app::{App, View};

/// Short display name for an attachment in status messages: the URL itself
/// for URL artifacts, the caption for notes (their `original` names an
/// origin like "typed", not a path), the *original* file's basename
/// otherwise (the stored copy's name carries a timestamp prefix nobody
/// typed).
fn attachment_label(a: &crate::tasks::store::Artifact) -> String {
    if a.kind == "url" {
        return a.path.clone();
    }
    if a.kind == "note" {
        return match &a.caption {
            Some(c) => format!("note — “{}”", c),
            None => "note".into(),
        };
    }
    std::path::Path::new(&a.original)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| a.original.clone())
}

impl App {
    /// `v` on a focused board card: open the Task Info popup (prompt +
    /// attachments). Returns false when no task is focused.
    pub fn enter_task_info(&mut self) -> bool {
        if self.selected_board_task().is_none() {
            return false;
        }
        self.tasks.info_sel = 0;
        self.render.task_info_scroll = 0;
        self.view = View::TaskInfo;
        true
    }

    pub fn close_task_info(&mut self) {
        self.tasks.info_sel = 0;
        self.render.task_info_scroll = 0;
        self.view = View::Grid;
    }

    /// The attachment under the Task Info popup cursor, if any. Used by the
    /// `c`/`o` keybinds to know what path to act on.
    pub fn selected_task_attachment(&self) -> Option<&crate::tasks::store::Artifact> {
        self.selected_board_task()?
            .artifacts
            .get(self.tasks.info_sel)
    }

    pub fn task_info_next(&mut self) {
        let n = self
            .selected_board_task()
            .map(|t| t.artifacts.len())
            .unwrap_or(0);
        if n == 0 {
            self.tasks.info_sel = 0;
            return;
        }
        self.tasks.info_sel = (self.tasks.info_sel + 1).min(n - 1);
    }

    pub fn task_info_prev(&mut self) {
        self.tasks.info_sel = self.tasks.info_sel.saturating_sub(1);
    }

    /// PgUp/PgDn handler for the Task Info popup. Negative steps scroll up;
    /// the renderer clamps the offset against content length.
    pub fn task_info_scroll_by(&mut self, delta: i32) {
        let cur = self.render.task_info_scroll as i32;
        self.render.task_info_scroll = (cur + delta).clamp(0, u16::MAX as i32) as u16;
    }

    /// `A` on a focused card (or `a` inside the Task Info popup): open the
    /// attach input, in note mode — Tab flips it to file-path/URL mode.
    /// Returns false when no task is focused.
    pub fn enter_task_attach(&mut self, from_info: bool) -> bool {
        let Some(t) = self.selected_board_task() else {
            return false;
        };
        self.tasks.attaching = Some(t.task_id.clone());
        self.tasks.attach_from_info = from_info;
        self.tasks.attach_note = true;
        self.tasks.input.clear();
        self.view = View::TaskAttachInput;
        true
    }

    /// Tab inside the attach input: flip between note and file-path/URL mode.
    /// The typed buffer is kept — starting in the wrong mode costs nothing.
    pub fn toggle_task_attach_mode(&mut self) {
        self.tasks.attach_note = !self.tasks.attach_note;
    }

    pub fn close_task_attach(&mut self) {
        self.tasks.input.clear();
        self.tasks.attaching = None;
        self.leave_task_attach();
    }

    /// Return from the attach input to where it was opened; true when that
    /// is the Task Info popup.
    fn leave_task_attach(&mut self) -> bool {
        let from_info = std::mem::take(&mut self.tasks.attach_from_info);
        self.view = if from_info {
            View::TaskInfo
        } else {
            View::Grid
        };
        from_info
    }

    /// Commit the attach input: in note mode write the text as a `note`
    /// attachment, otherwise copy the file (or record the URL) into the
    /// task's own artifacts dir via the shared op — then adopt the persisted
    /// state into the board. Empty input cancels. Returns false when nothing
    /// was attached (a failure lands in the persistence-error slot).
    pub fn submit_task_attach(&mut self) -> bool {
        let text = std::mem::take(&mut self.tasks.input);
        let raw = text.trim().to_string();
        let from_info = self.leave_task_attach();
        let Some(id) = self.tasks.attaching.take() else {
            return false;
        };
        if raw.is_empty() {
            return false;
        }
        let result = if self.tasks.attach_note {
            crate::ops::task::task_artifact_add_text(&id, &raw, "typed")
        } else {
            crate::ops::task::task_artifact_add(&id, &raw, None, None, false)
        };
        match result {
            Ok(state) => {
                let label = state
                    .artifacts
                    .last()
                    .map(attachment_label)
                    .unwrap_or_default();
                let n = state.artifacts.len();
                self.tasks.board.adopt(state);
                if from_info {
                    // Land the popup cursor on what was just attached.
                    self.tasks.info_sel = n.saturating_sub(1);
                }
                self.set_status(format!("attached {}", label));
                true
            }
            Err(e) => {
                self.tasks.record_persistence_error("attach", e);
                false
            }
        }
    }

    /// `x` inside the Task Info popup: remove the selected attachment
    /// (record + stored copy — personal tasks only, the store op refuses
    /// anything else). Returns the status message, or `None` when no task is
    /// focused.
    pub fn remove_selected_attachment(&mut self) -> Option<String> {
        let t = self.selected_board_task()?;
        if t.artifacts.is_empty() {
            return Some("no attachments to remove".into());
        }
        let id = t.task_id.clone();
        let idx = self.tasks.info_sel.min(t.artifacts.len() - 1);
        match crate::ops::task::task_artifact_remove(&id, idx) {
            Ok((state, removed)) => {
                let remaining = state.artifacts.len();
                self.tasks.board.adopt(state);
                self.tasks.info_sel = self.tasks.info_sel.min(remaining.saturating_sub(1));
                Some(format!("removed {}", attachment_label(&removed)))
            }
            Err(e) => Some(format!("attachment remove failed: {}", e)),
        }
    }

    /// `p` on a focused card or inside the Task Info popup: attach `text`
    /// (read from the clipboard by the bin-side key arm, so tests can inject
    /// anything) as a `note` attachment — a fresh `.md` file in the task's
    /// artifacts dir. Returns the status message, or `None` when no task is
    /// focused.
    pub fn attach_text_to_selected(&mut self, text: &str) -> Option<String> {
        let t = self.selected_board_task()?;
        let id = t.task_id.clone();
        if text.trim().is_empty() {
            return Some("clipboard empty — nothing attached".into());
        }
        match crate::ops::task::task_artifact_add_text(&id, text, "clipboard") {
            Ok(state) => {
                let caption = state
                    .artifacts
                    .last()
                    .and_then(|a| a.caption.clone())
                    .unwrap_or_default();
                let n = state.artifacts.len();
                self.tasks.board.adopt(state);
                if self.view == View::TaskInfo {
                    // Land the popup cursor on what was just pasted.
                    self.tasks.info_sel = n.saturating_sub(1);
                }
                Some(format!("attached note — “{}”", caption))
            }
            Err(e) => Some(format!("attach failed: {}", e)),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use crate::app::test_support::{status, task_app, tasks};
    use crate::app::TasksCommand;

    /// A throwaway source file outside the (redirected) home, like a real
    /// research doc an agent left in some working dir.
    fn temp_doc(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cchub-attach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn attach_copies_file_into_personal_store() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("research the cache").unwrap().unwrap();
            app.focus_task(&id);
            let doc = temp_doc("research.md", "# findings\ncache is cold\n");

            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
            assert_eq!(app.view, crate::app::View::TaskAttachInput);
            // The input opens in note mode; Tab over to file/URL mode.
            app.toggle_task_attach_mode();
            app.tasks.input = doc.display().to_string();
            tasks(&mut app, TasksCommand::SubmitAttach);

            assert_eq!(app.view, crate::app::View::Grid);
            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.artifacts.len(), 1);
            let a = &t.artifacts[0];
            assert_eq!(a.kind, "file");
            assert_eq!(a.original, doc.display().to_string());
            let stored = std::path::PathBuf::from(&a.path);
            assert!(
                stored.starts_with(dirs::home_dir().unwrap().join(".cc-hub/tasks").join(&id)),
                "copy must land inside the task's own dir, got {}",
                a.path
            );
            assert_eq!(
                std::fs::read_to_string(&stored).unwrap(),
                "# findings\ncache is cold\n"
            );
            assert!(
                status(&app).starts_with("attached research.md"),
                "got: {}",
                status(&app)
            );
            // Disk round-trip: a fresh board load sees the attachment too.
            assert_eq!(
                crate::tasks::PersonalBoard::load()
                    .get(&id)
                    .unwrap()
                    .artifacts
                    .len(),
                1
            );
            std::fs::remove_file(&doc).ok();
        });
    }

    #[test]
    fn attach_url_records_verbatim() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("read the RFC").unwrap().unwrap();
            app.focus_task(&id);

            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
            app.toggle_task_attach_mode();
            // Trailing whitespace mimics a sloppy paste; submit trims it.
            app.tasks.input = "https://example.com/rfc  ".into();
            tasks(&mut app, TasksCommand::SubmitAttach);

            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.artifacts.len(), 1);
            assert_eq!(t.artifacts[0].kind, "url");
            assert_eq!(t.artifacts[0].path, "https://example.com/rfc");
        });
    }

    #[test]
    fn attach_default_mode_types_note() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("jot it down").unwrap().unwrap();
            app.focus_task(&id);

            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
            assert!(app.tasks.attach_note, "attach input must open in note mode");
            app.tasks.input = "check the cache TTL first".into();
            tasks(&mut app, TasksCommand::SubmitAttach);

            assert_eq!(app.view, crate::app::View::Grid);
            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.artifacts.len(), 1);
            let a = &t.artifacts[0];
            assert_eq!(a.kind, "note");
            assert_eq!(a.original, "typed");
            assert_eq!(a.caption.as_deref(), Some("check the cache TTL first"));
            assert_eq!(
                std::fs::read_to_string(&a.path).unwrap(),
                "check the cache TTL first"
            );
            assert!(
                status(&app).starts_with("attached note — “check the cache TTL first”"),
                "got: {}",
                status(&app)
            );
        });
    }

    #[test]
    fn attach_mode_toggles_and_resets_on_reopen() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("mode memory").unwrap().unwrap();
            app.focus_task(&id);

            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
            app.tasks.input = "half-typed".into();
            app.toggle_task_attach_mode();
            assert!(!app.tasks.attach_note);
            assert_eq!(app.tasks.input, "half-typed", "buffer survives the flip");
            app.toggle_task_attach_mode();
            assert!(app.tasks.attach_note);

            // A path-mode session doesn't leak into the next open.
            app.toggle_task_attach_mode();
            app.close_task_attach();
            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
            assert!(app.tasks.attach_note, "reopen must start back in note mode");
            app.close_task_attach();
        });
    }

    #[test]
    fn attach_empty_input_cancels() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("nothing to see").unwrap().unwrap();
            app.focus_task(&id);
            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
            tasks(&mut app, TasksCommand::SubmitAttach);
            assert_eq!(app.view, crate::app::View::Grid);
            assert!(app.tasks.board.get(&id).unwrap().artifacts.is_empty());
            assert_eq!(status(&app), "attach cancelled — empty input");
        });
    }

    #[test]
    fn attach_missing_file_surfaces_error() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("bad path").unwrap().unwrap();
            app.focus_task(&id);
            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
            app.toggle_task_attach_mode();
            app.tasks.input = "/nonexistent/research.md".into();
            tasks(&mut app, TasksCommand::SubmitAttach);
            assert!(app.tasks.board.get(&id).unwrap().artifacts.is_empty());
            assert!(
                status(&app).starts_with("task attach failed"),
                "got: {}",
                status(&app)
            );
        });
    }

    #[test]
    fn task_info_remove_deletes_file_and_fixes_lead() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("two docs").unwrap().unwrap();
            app.focus_task(&id);
            let doc1 = temp_doc("first.md", "one\n");
            let doc2 = temp_doc("second.md", "two\n");
            for doc in [&doc1, &doc2] {
                tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
                app.toggle_task_attach_mode();
                app.tasks.input = doc.display().to_string();
                tasks(&mut app, TasksCommand::SubmitAttach);
            }
            // Mark the second attachment as lead directly in the store; the
            // removal below must shift the designation, not drop it.
            crate::tasks::store::update_task(&id, |s| s.lead_artifact = Some(1)).unwrap();
            app.tasks.reload();
            app.focus_task(&id);

            tasks(&mut app, TasksCommand::OpenTaskInfo);
            assert_eq!(app.view, crate::app::View::TaskInfo);
            assert_eq!(app.tasks.info_sel, 0);
            let stored0 =
                std::path::PathBuf::from(&app.tasks.board.get(&id).unwrap().artifacts[0].path);

            tasks(&mut app, TasksCommand::RemoveAttachment);
            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.artifacts.len(), 1);
            assert!(t.artifacts[0].original.ends_with("second.md"));
            assert_eq!(
                t.lead_artifact,
                Some(0),
                "lead must follow the shifted slot"
            );
            assert!(!stored0.exists(), "stored copy must be deleted");
            assert!(
                status(&app).starts_with("removed first.md"),
                "got: {}",
                status(&app)
            );
            std::fs::remove_file(&doc1).ok();
            std::fs::remove_file(&doc2).ok();
        });
    }

    #[test]
    fn attach_from_info_returns_to_popup_and_selects_new() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("popup flow").unwrap().unwrap();
            app.focus_task(&id);
            let doc1 = temp_doc("a.md", "a\n");
            let doc2 = temp_doc("b.md", "b\n");
            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: false });
            app.toggle_task_attach_mode();
            app.tasks.input = doc1.display().to_string();
            tasks(&mut app, TasksCommand::SubmitAttach);

            tasks(&mut app, TasksCommand::OpenTaskInfo);
            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: true });
            assert_eq!(app.view, crate::app::View::TaskAttachInput);
            app.toggle_task_attach_mode();
            app.tasks.input = doc2.display().to_string();
            tasks(&mut app, TasksCommand::SubmitAttach);
            assert_eq!(app.view, crate::app::View::TaskInfo);
            assert_eq!(app.tasks.info_sel, 1, "cursor lands on the new attachment");

            // Esc from an info-opened attach input returns to the popup too.
            tasks(&mut app, TasksCommand::OpenAttachInput { from_info: true });
            app.close_task_attach();
            assert_eq!(app.view, crate::app::View::TaskInfo);
            std::fs::remove_file(&doc1).ok();
            std::fs::remove_file(&doc2).ok();
        });
    }

    #[test]
    fn paste_text_attaches_note() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("collect findings").unwrap().unwrap();
            app.focus_task(&id);

            let msg = app
                .attach_text_to_selected("# Cache research\nthe cache is cold\n")
                .unwrap();
            assert!(msg.starts_with("attached note"), "got: {}", msg);

            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.artifacts.len(), 1);
            let a = &t.artifacts[0];
            assert_eq!(a.kind, "note");
            assert_eq!(a.original, "clipboard");
            assert_eq!(a.caption.as_deref(), Some("# Cache research"));
            let stored = std::path::PathBuf::from(&a.path);
            assert!(
                stored.starts_with(dirs::home_dir().unwrap().join(".cc-hub/tasks").join(&id)),
                "note must land inside the task's own dir, got {}",
                a.path
            );
            assert!(stored.extension().is_some_and(|e| e == "md"));
            assert_eq!(
                std::fs::read_to_string(&stored).unwrap(),
                "# Cache research\nthe cache is cold\n"
            );
            // Round-trip through a fresh load.
            assert_eq!(
                crate::tasks::PersonalBoard::load()
                    .get(&id)
                    .unwrap()
                    .artifacts
                    .len(),
                1
            );
        });
    }

    #[test]
    fn paste_empty_clipboard_attaches_nothing() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("empty paste").unwrap().unwrap();
            app.focus_task(&id);
            let msg = app.attach_text_to_selected("   \n").unwrap();
            assert_eq!(msg, "clipboard empty — nothing attached");
            assert!(app.tasks.board.get(&id).unwrap().artifacts.is_empty());
        });
    }

    #[test]
    fn paste_notes_in_popup_get_distinct_files_and_selection() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            let id = app.tasks.board.add("two notes").unwrap().unwrap();
            app.focus_task(&id);
            tasks(&mut app, TasksCommand::OpenTaskInfo);

            app.attach_text_to_selected("first note").unwrap();
            app.attach_text_to_selected("second note").unwrap();
            assert_eq!(app.tasks.info_sel, 1, "cursor follows the newest note");

            let t = app.tasks.board.get(&id).unwrap();
            assert_eq!(t.artifacts.len(), 2);
            // Both notes were almost certainly written in the same second —
            // the collision probe must have kept their files apart.
            assert_ne!(t.artifacts[0].path, t.artifacts[1].path);
            assert_eq!(
                std::fs::read_to_string(&t.artifacts[0].path).unwrap(),
                "first note"
            );
            assert_eq!(
                std::fs::read_to_string(&t.artifacts[1].path).unwrap(),
                "second note"
            );
        });
    }

    #[test]
    fn task_info_without_focus_reports() {
        crate::test_util::with_temp_home(|| {
            let (mut app, _rt) = task_app();
            tasks(&mut app, TasksCommand::OpenTaskInfo);
            assert_eq!(app.view, crate::app::View::Grid);
            assert_eq!(status(&app), "no task focused");
        });
    }
}
