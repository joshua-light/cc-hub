//! Binding a card to an agent: the assign picker (`s`), quick-assign at
//! `$HOME` (`S`), the plan-first spawn, and resume.

use crate::app::{App, View};
use crate::config;
use crate::folder_picker::FolderPicker;
use crate::tasks::store::{TaskState, TaskStatus};
use std::path::PathBuf;

/// Wrap a board task's text in plan-first framing: the agent investigates
/// and presents a plan, then holds until the user approves (Space sends
/// [`PROCEED_PROMPT`](super::PROCEED_PROMPT)). This is what makes the Planning column honest — the
/// agent genuinely isn't implementing while the card sits there.
pub(super) fn planning_prompt(task: &TaskState) -> String {
    let mut prompt = task.prompt.clone();
    for note in task_notes(task) {
        prompt.push_str("\n\n");
        prompt.push_str(&note);
    }
    prompt.push_str(
        "\n\nFirst investigate the codebase and figure out what this task needs, \
         then present a short implementation plan: approach, files you'll touch, open \
         questions. Do NOT implement yet — stop after the plan and wait for me to say \
         \"proceed\".",
    );
    prompt
}

/// Longest a note is quoted into the planning prompt before the agent is
/// pointed at the file instead. A pasted spec or stack trace belongs in the
/// prompt; a pasted logfile belongs on disk, one Read away.
const NOTE_PROMPT_BUDGET: usize = 2000;

/// The task's `note` attachments (context pasted in the add popup, or `p`
/// pasted onto the card later) as prompt sections, in attach order. Each is
/// labelled with the file it came from, so the agent can read the rest of a
/// note that was too long to quote — and unreadable notes are skipped rather
/// than failing the spawn.
fn task_notes(task: &TaskState) -> Vec<String> {
    task.artifacts
        .iter()
        .filter(|a| a.kind == "note")
        .filter_map(|a| {
            let text = std::fs::read_to_string(&a.path).ok()?;
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            Some(match text.char_indices().nth(NOTE_PROMPT_BUDGET) {
                None => format!("Context ({}):\n{}", a.path, text),
                Some((cut, _)) => format!(
                    "Context (first {} chars of {} — read the file for the rest):\n{}",
                    NOTE_PROMPT_BUDGET,
                    a.path,
                    &text[..cut]
                ),
            })
        })
        .collect()
}

impl App {
    /// `s` on a focused task: open the places picker (bookmarks and recent
    /// dirs, fuzzy-filterable) to choose the cwd the agent will run in,
    /// falling back to the filesystem browser when nothing is known yet.
    /// Returns false when no task is focused or the task is already Done.
    pub fn enter_task_assign_picker(&mut self) -> bool {
        let Some(t) = self.selected_board_task() else {
            return false;
        };
        if t.status == TaskStatus::Done {
            return false;
        }
        let id = t.task_id.clone();
        let prev_cwd = t.cwd.clone();
        let places = self.assign_places();
        self.tasks.pending_assign = Some(id);
        if places.is_empty() {
            self.folder_picker = Some(FolderPicker::new(Self::assign_browse_start(
                prev_cwd.as_deref(),
            )));
        } else {
            let mut picker = FolderPicker::new_places(places);
            if let Some(cwd) = prev_cwd.as_deref() {
                picker.select_path(std::path::Path::new(cwd));
            }
            self.folder_picker = Some(picker);
        }
        self.view = View::FolderPicker;
        true
    }

    /// `S` on a focused task: skip the picker and spawn the agent right
    /// away with `$HOME` as the cwd — for broad questions not tied to any
    /// project yet; where to go next gets figured out with the agent.
    /// Returns None when no task is focused or the task is already Done.
    pub fn assign_selected_task_at_home(&mut self) -> Option<String> {
        let t = self.selected_board_task()?;
        if t.status == TaskStatus::Done {
            return None;
        }
        let id = t.task_id.clone();
        self.tasks.pending_assign = Some(id);
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        Some(self.assign_task_agent(&home.display().to_string()))
    }

    /// Picker chose `cwd` for the pending assignment: spawn the default
    /// agent there, hand it the task text wrapped in plan-first framing
    /// ([`planning_prompt`] — inline when the agent supports a spawn-time
    /// prompt, otherwise queued for dispatch once the session reports Idle —
    /// Claude ignores spawn-time prompts), record the binding, and move the
    /// card to Planning. Space on the card later approves the plan and
    /// promotes it to In Progress. Returns the status line.
    pub fn assign_task_agent(&mut self, cwd: &str) -> String {
        let pending = self.tasks.pending_assign.take();
        self.close_folder_picker();
        let Some(id) = pending else {
            return "no task pending assignment".into();
        };
        let Some(task) = self.tasks.board.get(&id) else {
            return "task vanished before assignment".into();
        };
        let prompt = planning_prompt(task);
        let title = task.session_title();
        let agent_id = config::get().default_session_agent_id();
        let supports_initial_prompt = config::get()
            .agent(&agent_id)
            .is_some_and(|a| a.supports_initial_prompt());
        match self.runtime.spawn_session(
            &agent_id,
            cwd,
            None,
            supports_initial_prompt.then_some(prompt.as_str()),
            None,
            false,
        ) {
            Ok(tmux) => {
                if !supports_initial_prompt {
                    self.queue_pending_dispatch(tmux.clone(), prompt);
                }
                if let Err(e) = self.tasks.board.assign(&id, cwd, &agent_id, &tmux) {
                    let cleanup = self
                        .runtime
                        .kill_session(&tmux)
                        .err()
                        .map(|cleanup| format!("; cleanup failed: {cleanup}"))
                        .unwrap_or_default();
                    return format!("assign rolled back: task write failed: {e}{cleanup}");
                }
                // Named after its card the moment the scanner sees it; see
                // `adopt_pending_spawn_names`.
                self.pending_spawn_names.insert(tmux.clone(), Some(title));
                self.focus_task(&id);
                format!(
                    "assigned {} [{}] — planning (Space approves the plan)",
                    agent_id, tmux
                )
            }
            Err(e) => format!("assign failed: {}", e),
        }
    }

    pub fn task_session_is_live(&self, tmux: &str) -> bool {
        self.runtime.session_exists(tmux)
    }

    /// Respawn the card's agent resuming its recorded session, and point
    /// the card's binding at the new mux session. Returns its name.
    pub fn resume_board_task(&mut self, task: &TaskState) -> Result<String, String> {
        let sid = task
            .session_id
            .clone()
            .ok_or_else(|| "task has no resumable session id".to_string())?;
        let cwd = task
            .cwd
            .as_deref()
            .ok_or_else(|| "task has no assignment directory".to_string())?;
        let agent_id = task.agent_id.as_deref().unwrap_or("claude");
        let tmux = self
            .runtime
            .spawn_session(
                agent_id,
                cwd,
                Some(crate::spawn::SessionTarget::Resume(sid)),
                None,
                None,
                false,
            )
            .map_err(|e| format!("resume failed: {e}"))?;
        if let Err(e) = self.tasks.board.rebind_tmux(&task.task_id, &tmux) {
            let _ = self.runtime.kill_session(&tmux);
            return Err(format!("resume rolled back: task write failed: {e}"));
        }
        Ok(tmux)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::agent_runtime::testing::RecordingRuntime;
    use crate::test_util::with_temp_home;
    use std::sync::Arc;

    // A session the board starts for a card is named after it, instead of
    // arriving nameless and asking.
    #[test]
    fn assigned_session_is_named_after_its_card() {
        crate::test_util::with_temp_home(|| {
            let runtime = Arc::new(RecordingRuntime::default());
            let mut app = App::new_with_runtime(runtime.clone());
            let id = app.tasks.board.add("implement it").unwrap().unwrap();
            app.tasks.pending_assign = Some(id);

            app.assign_task_agent("/tmp");

            assert_eq!(
                app.pending_spawn_names.get("mock-spawn"),
                Some(&Some("Task: implement it".into()))
            );
        });
    }

    #[test]
    fn an_oversized_note_is_quoted_up_to_the_budget() {
        with_temp_home(|| {
            let mut app = App::new();
            app.enter_task_input();
            app.tasks.input = "big one".into();
            app.tasks.context = "x".repeat(NOTE_PROMPT_BUDGET + 500);
            assert!(app.submit_task_input());

            let t = app.selected_board_task().unwrap().clone();
            let prompt = planning_prompt(&t);
            assert!(prompt.contains("read the file for the rest"));
            // Exactly the budget is quoted — the temp-dir path in the
            // header can carry an `x` of its own, so bound the run
            // rather than counting characters across the whole prompt.
            assert!(prompt.contains(&"x".repeat(NOTE_PROMPT_BUDGET)));
            assert!(!prompt.contains(&"x".repeat(NOTE_PROMPT_BUDGET + 1)));
            // The full text is still on disk for the agent to open.
            let stored = std::fs::read_to_string(&t.artifacts[0].path).unwrap();
            assert_eq!(stored.len(), NOTE_PROMPT_BUDGET + 500);
        });
    }
}
