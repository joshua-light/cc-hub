//! Personal task board shown on the Tasks tab. Unlike the Projects-tab
//! orchestrator tasks (heavyweight: orchestrator session, workers, PR
//! pipeline), a board task is a plain to-do item that can optionally be
//! handed to a single agent session: assigning spawns a detached agent in a
//! chosen cwd, prompted to investigate and plan first — the card sits in
//! Planning until the user approves the plan (Space), which tells the agent
//! to proceed and moves the card to In Progress. The binding is recorded so
//! `f` on the card attaches to that session exactly like the Sessions tab.
//!
//! Since the task-model unification a board task IS an
//! [`orchestrator::TaskState`] with `project_id: None`, stored one file per
//! task at `~/.cc-hub/tasks/<task-id>/state.json` — the same per-task
//! lock + tempfile-rename machinery as the Projects store, with the legal
//! status edges enforced by the shared transition table. [`PersonalBoard`]
//! is the in-memory snapshot the TUI mutates through; every mutation is a
//! locked read-mutate-write of the task's own file, so concurrent cc-hub
//! instances conflict per task, not per board. Board-level metadata
//! (`last_assign_cwd`) lives in `~/.cc-hub/board.json`.
//!
//! The pre-unification single-file board (`~/.cc-hub/tasks.json`) is
//! migrated automatically on first load — see [`migrate_legacy_board`].

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io;
use std::path::PathBuf;

use crate::orchestrator::{
    self, personal_task_dir, personal_tasks_dir, read_task_state_for, update_personal_task,
    write_task_state, TaskPriority, TaskState, TaskStatus,
};
use crate::platform::paths::cc_hub_home;

/// Longest a single tag may be after normalization; longer ones are truncated.
const MAX_TAG_LEN: usize = 16;
/// Most tags a task may carry; extras are dropped so the border badge fits.
const MAX_TAGS: usize = 6;

/// Parse a free-text tag line (from the inline editor) into the normalized set
/// stored on a task. Splits on whitespace *and* commas, strips a leading `#`,
/// lowercases, trims, drops empties, and dedupes (keeping first-seen order).
/// Each tag is capped at [`MAX_TAG_LEN`] chars and the set at [`MAX_TAGS`], so
/// the card's border badge always has a sane bound. Editing replaces the whole
/// set, so this is the single chokepoint for what a task's tags can be.
pub fn parse_tags(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in input.split([',', ' ', '\t', '\n']) {
        let tag = raw.trim().trim_start_matches('#').trim().to_lowercase();
        if tag.is_empty() {
            continue;
        }
        let tag: String = tag.chars().take(MAX_TAG_LEN).collect();
        if out.iter().any(|t| t == &tag) {
            continue;
        }
        out.push(tag);
        if out.len() >= MAX_TAGS {
            break;
        }
    }
    out
}

/// Parsed form of the add popup's quick syntax: `#word` tokens become tags
/// and a `!1`–`!4` token sets the priority; everything else stays as the
/// task text. Rename leaves text verbatim — the syntax is capture-time
/// sugar only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickAdd {
    pub text: String,
    pub tags: Vec<String>,
    pub priority: Option<TaskPriority>,
}

/// Split the add popup's buffer into text, tags, and priority. Tag tokens
/// run through [`parse_tags`], so the tag editor's normalization and caps
/// apply here too. Several `!N` tokens keep the last one; a bare `#` or an
/// out-of-range `!5` is ordinary text.
pub fn parse_quick_add(input: &str) -> QuickAdd {
    let mut words: Vec<&str> = Vec::new();
    let mut tag_words: Vec<&str> = Vec::new();
    let mut priority = None;
    for tok in input.split_whitespace() {
        if let Some(rest) = tok.strip_prefix('#') {
            if !rest.is_empty() {
                tag_words.push(rest);
                continue;
            }
        }
        match tok {
            "!1" => priority = Some(TaskPriority::P1),
            "!2" => priority = Some(TaskPriority::P2),
            "!3" => priority = Some(TaskPriority::P3),
            "!4" => priority = Some(TaskPriority::P4),
            _ => words.push(tok),
        }
    }
    QuickAdd {
        text: words.join(" "),
        tags: parse_tags(&tag_words.join(" ")),
        priority,
    }
}

/// Board-level metadata that isn't per-task: currently just the cwd of the
/// most recent assignment, kept across restarts so the assign picker promotes
/// it to the top of the places list — firing several tasks at one project is
/// a plain Enter each time.
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
struct BoardMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_assign_cwd: Option<String>,
}

fn board_meta_path() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("board.json"))
}

fn load_board_meta() -> BoardMeta {
    let Some(path) = board_meta_path() else {
        return BoardMeta::default();
    };
    fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_board_meta(meta: &BoardMeta) -> io::Result<()> {
    let Some(path) = board_meta_path() else {
        return Ok(());
    };
    crate::persist::save_json(&path, meta)
}

/// In-memory snapshot of the personal board: every `~/.cc-hub/tasks/<id>/`
/// task, ordered by `created_at` (ties broken by id, which embeds nanos).
/// Mutations write through the per-task locked store and update the snapshot
/// from the state the write returned, so what the TUI shows is what landed.
#[derive(Default, Debug)]
pub struct PersonalBoard {
    tasks: Vec<TaskState>,
    last_assign_cwd: Option<String>,
}

impl PersonalBoard {
    /// Load the board from disk. A missing store is an empty board; an
    /// unreadable or malformed task file is an error so callers never
    /// mistake data loss for an intentionally empty board.
    pub fn load_result() -> io::Result<Self> {
        let mut tasks = Vec::new();
        if let Some(dir) = personal_tasks_dir() {
            match fs::read_dir(&dir) {
                Ok(entries) => {
                    for entry in entries {
                        let entry = entry?;
                        if !entry.file_type()?.is_dir() {
                            continue;
                        }
                        let task_id = entry.file_name().to_string_lossy().into_owned();
                        tasks.push(read_task_state_for(None, &task_id)?);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        tasks.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.task_id.cmp(&b.task_id))
        });
        Ok(Self {
            tasks,
            last_assign_cwd: load_board_meta().last_assign_cwd,
        })
    }

    /// Convenience for non-interactive callers and tests. Runtime UI code uses
    /// [`Self::load_result`] so it can surface failures.
    pub fn load() -> Self {
        Self::load_result().unwrap_or_default()
    }

    pub fn tasks(&self) -> &[TaskState] {
        &self.tasks
    }

    pub fn get(&self, id: &str) -> Option<&TaskState> {
        self.tasks.iter().find(|t| t.task_id == id)
    }

    pub fn last_assign_cwd(&self) -> Option<&str> {
        self.last_assign_cwd.as_deref()
    }

    /// Tasks in `status`, in board order.
    pub fn column(&self, status: TaskStatus) -> Vec<&TaskState> {
        self.tasks.iter().filter(|t| t.status == status).collect()
    }

    /// Append a task (trimmed) to To-Do. Empty/whitespace-only input is
    /// ignored. Returns the new task's id. Persists immediately.
    pub fn add(&mut self, text: &str) -> io::Result<Option<String>> {
        self.add_configured(text, Vec::new(), TaskPriority::default())
    }

    /// Add a task and its quick-capture metadata in one write, so a failure
    /// cannot leave behind a card missing its tags/priority.
    pub fn add_configured(
        &mut self,
        text: &str,
        tags: Vec<String>,
        priority: TaskPriority,
    ) -> io::Result<Option<String>> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        let mut state = TaskState::new_personal(text.to_string());
        state.tags = tags;
        state.priority = priority;
        write_task_state(&state)?;
        let id = state.task_id.clone();
        self.tasks.push(state);
        Ok(Some(id))
    }

    /// Replace a task's text (trimmed), leaving status and any agent
    /// binding untouched. Empty/whitespace-only input and unknown ids are
    /// ignored (returns false). Persists.
    pub fn rename(&mut self, id: &str, text: &str) -> io::Result<bool> {
        let text = text.trim();
        if text.is_empty() || self.get(id).is_none_or(|t| t.prompt == text) {
            return Ok(false);
        }
        let updated = update_personal_task(id, |s| s.prompt = text.to_string())?;
        self.apply(updated);
        Ok(true)
    }

    /// Set a task's priority. Skips the disk write when the priority is
    /// unchanged (re-pressing the same level is a no-op). No-op on unknown
    /// id. Persists when it changes.
    pub fn set_priority(&mut self, id: &str, priority: TaskPriority) -> io::Result<bool> {
        if self.get(id).is_none_or(|t| t.priority == priority) {
            return Ok(false);
        }
        let updated = update_personal_task(id, |s| s.priority = priority)?;
        self.apply(updated);
        Ok(true)
    }

    /// Replace a task's tags with `tags` (already normalized by
    /// [`parse_tags`]). Skips the disk write when unchanged. No-op on unknown
    /// id. Persists when it changes.
    pub fn set_tags(&mut self, id: &str, tags: Vec<String>) -> io::Result<bool> {
        if self.get(id).is_none_or(|t| t.tags == tags) {
            return Ok(false);
        }
        let updated = update_personal_task(id, |s| s.tags = tags)?;
        self.apply(updated);
        Ok(true)
    }

    /// Move a task between columns, stamping/clearing `done_at` so the Done
    /// column can show when it landed. No-op on unknown id. Persists. The
    /// shared transition table validates the edge, so an illegal move (e.g.
    /// from a hand-edited state file) errors instead of silently landing.
    pub fn set_status(&mut self, id: &str, status: TaskStatus) -> io::Result<bool> {
        if self.get(id).is_none_or(|t| t.status == status) {
            return Ok(false);
        }
        let updated = update_personal_task(id, |s| {
            s.status = status;
            s.done_at = (status == TaskStatus::Done).then(orchestrator::now_unix_secs);
        })?;
        self.apply(updated);
        Ok(true)
    }

    /// Record an agent assignment: where it runs, which agent, and the mux
    /// session it lives in. Moves the task to Planning — the agent is
    /// prompted to plan first and the user promotes the card to In Progress
    /// by approving the plan. Persists.
    pub fn assign(&mut self, id: &str, cwd: &str, agent_id: &str, tmux: &str) -> io::Result<bool> {
        if self.get(id).is_none() {
            return Ok(false);
        }
        let updated = update_personal_task(id, |s| {
            s.cwd = Some(cwd.to_string());
            s.agent_id = Some(agent_id.to_string());
            s.tmux = Some(tmux.to_string());
            // A re-assign spawns a fresh session; the old session id no
            // longer matches the new tmux, so drop it until the next scan
            // re-resolves.
            s.session_id = None;
            s.status = TaskStatus::Planning;
            s.done_at = None;
        })?;
        self.apply(updated);
        self.last_assign_cwd = Some(cwd.to_string());
        save_board_meta(&BoardMeta {
            last_assign_cwd: self.last_assign_cwd.clone(),
        })?;
        Ok(true)
    }

    /// Point an existing assignment at a new mux session (resume after the
    /// old tmux died). Keeps `session_id` — resume continues that session.
    pub fn rebind_tmux(&mut self, id: &str, tmux: &str) -> io::Result<bool> {
        if self.get(id).is_none_or(|t| t.tmux.as_deref() == Some(tmux)) {
            return Ok(false);
        }
        let updated = update_personal_task(id, |s| s.tmux = Some(tmux.to_string()))?;
        self.apply(updated);
        Ok(true)
    }

    /// Fill in `session_id` for any assigned task whose tmux name matches a
    /// scanned session. Returns true when something new was learned (and
    /// persisted) so callers can repaint.
    pub fn bind_sessions(&mut self, sessions: &[crate::models::SessionInfo]) -> io::Result<bool> {
        let bindings: Vec<(String, String)> = self
            .tasks
            .iter()
            .filter_map(|t| {
                if t.session_id.is_some() {
                    return None;
                }
                let tmux = t.tmux.as_deref()?;
                sessions
                    .iter()
                    .find(|s| s.tmux_session.as_deref() == Some(tmux))
                    .map(|s| (t.task_id.clone(), s.session_id.clone()))
            })
            .collect();
        if bindings.is_empty() {
            return Ok(false);
        }
        for (id, sid) in bindings {
            let updated = update_personal_task(&id, |s| s.session_id = Some(sid.clone()))?;
            self.apply(updated);
        }
        Ok(true)
    }

    /// Remove the task with `id`, returning it so the caller can describe
    /// what was deleted (and whether an agent session survives it). The
    /// removed task is appended to the on-disk archive. Persists.
    pub fn remove(&mut self, id: &str) -> io::Result<Option<TaskState>> {
        let Some(idx) = self.tasks.iter().position(|t| t.task_id == id) else {
            return Ok(None);
        };
        let removed = self.tasks[idx].clone();
        // Archive first: an extra archive entry is harmless, while removing
        // the task dir before a failed archive write would report failure
        // after the card had already disappeared.
        archive_tasks(std::slice::from_ref(&removed))?;
        if let Some(dir) = personal_task_dir(id) {
            match fs::remove_dir_all(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        self.tasks.remove(idx);
        Ok(Some(removed))
    }

    /// Re-insert a previously removed task (undo of `x`/`c`). Skips ids
    /// already on the board (double-undo, hand edits); returns whether it
    /// landed. Persists.
    pub fn restore(&mut self, item: TaskState) -> io::Result<bool> {
        if self.get(&item.task_id).is_some() {
            return Ok(false);
        }
        write_task_state(&item)?;
        self.tasks.push(item);
        Ok(true)
    }

    /// Drop every Done task, preserving the order of the rest. The removed
    /// tasks are appended to the on-disk archive and returned so the caller
    /// can offer undo. Only persists when something actually changed.
    pub fn clear_done(&mut self) -> io::Result<Vec<TaskState>> {
        let done: Vec<TaskState> = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Done)
            .cloned()
            .collect();
        if done.is_empty() {
            return Ok(done);
        }
        archive_tasks(&done)?;
        for t in &done {
            if let Some(dir) = personal_task_dir(&t.task_id) {
                match fs::remove_dir_all(&dir) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
        self.tasks.retain(|t| t.status != TaskStatus::Done);
        Ok(done)
    }

    /// Adopt the state a locked write performed *outside* the board returned
    /// (e.g. the attach/remove artifact ops in `ops::task`), so the snapshot
    /// shows what landed without a full disk reload.
    pub(crate) fn adopt(&mut self, updated: TaskState) {
        self.apply(updated);
    }

    /// Replace the in-memory copy of a task with the state a locked write
    /// returned, so the snapshot always shows what actually landed on disk.
    fn apply(&mut self, updated: TaskState) {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.task_id == updated.task_id) {
            *t = updated;
        }
    }
}

fn archive_path() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("tasks-archive-v2.json"))
}

/// Append removed tasks to `~/.cc-hub/tasks-archive-v2.json` (a bare JSON
/// array of unified [`TaskState`]s), so `x` and `c` are recoverable beyond
/// the in-session undo slot. The archive is a log, not a ledger: an undone
/// delete leaves its copy behind, and a corrupt file starts fresh — same
/// policy as the board. The pre-unification `tasks-archive.json` (legacy
/// `TaskItem` shape) is left untouched.
fn archive_tasks(items: &[TaskState]) -> io::Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let Some(path) = archive_path() else {
        return Ok(());
    };
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("task archive path has no parent"))?;
    fs::create_dir_all(parent)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(parent.join("tasks-archive.lock"))?;
    lock.lock_exclusive()?;
    let mut archived: Vec<TaskState> = match fs::read_to_string(&path) {
        Ok(raw) => {
            serde_json::from_str(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    archived.extend(items.iter().cloned());
    crate::persist::save_json(&path, &archived)
}

/// Frozen serde shapes of the pre-unification board, kept only so
/// [`migrate_legacy_board`] can parse an existing `~/.cc-hub/tasks.json`
/// byte-for-byte the way the old code did. Never construct these outside
/// migration.
pub mod legacy {
    use serde::Deserialize;

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum TaskItemStatus {
        Todo,
        Planning,
        InProgress,
        Done,
    }

    #[derive(Clone, Debug, Deserialize)]
    pub struct TaskItem {
        pub id: String,
        pub text: String,
        pub status: TaskItemStatus,
        #[serde(default)]
        pub priority: super::TaskPriority,
        #[serde(default)]
        pub tags: Vec<String>,
        #[serde(default)]
        pub created_at: u64,
        #[serde(default)]
        pub done_at: Option<u64>,
        #[serde(default)]
        pub cwd: Option<String>,
        #[serde(default)]
        pub agent_id: Option<String>,
        #[serde(default)]
        pub tmux: Option<String>,
        #[serde(default)]
        pub session_id: Option<String>,
    }

    #[derive(Default, Debug, Deserialize)]
    pub struct TaskBoard {
        #[serde(default)]
        pub tasks: Vec<TaskItem>,
        #[serde(default)]
        pub last_assign_cwd: Option<String>,
        #[serde(default)]
        pub revision: u64,
    }
}

fn legacy_tasks_path() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("tasks.json"))
}

fn legacy_item_to_state(item: legacy::TaskItem) -> TaskState {
    let mut state = TaskState::new_personal(item.text);
    state.task_id = item.id;
    state.status = match item.status {
        legacy::TaskItemStatus::Todo => TaskStatus::Backlog,
        legacy::TaskItemStatus::Planning => TaskStatus::Planning,
        legacy::TaskItemStatus::InProgress => TaskStatus::Running,
        legacy::TaskItemStatus::Done => TaskStatus::Done,
    };
    state.priority = item.priority;
    state.tags = item.tags;
    state.created_at = item.created_at as i64;
    state.done_at = item.done_at.map(|t| t as i64);
    state.updated_at = (item.created_at.max(item.done_at.unwrap_or(0))) as i64;
    state.cwd = item.cwd;
    state.agent_id = item.agent_id;
    state.tmux = item.tmux;
    state.session_id = item.session_id;
    state
}

/// Migrate a pre-unification `~/.cc-hub/tasks.json` into the per-task store.
/// Lossless, idempotent, and abort-on-error:
///
/// 1. No `tasks.json` → nothing to do (the disarmed trigger).
/// 2. The old board lock (`tasks.lock`) is held throughout, serializing
///    against a concurrently running pre-unification binary.
/// 3. A parse failure aborts touching nothing — the caller surfaces the
///    error and the file stays exactly as it was.
/// 4. Tasks whose directory already exists are skipped (re-run safety after
///    a partial migration).
/// 5. Only after every task is written: `last_assign_cwd` lands in
///    `board.json` and `tasks.json` is renamed to `tasks.json.migrated-v1` —
///    the backup doubles as the rollback story.
///
/// Returns the number of migrated tasks, or `None` when there was nothing
/// to migrate.
pub fn migrate_legacy_board() -> io::Result<Option<usize>> {
    let Some(path) = legacy_tasks_path() else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("tasks path has no parent"))?;
    fs::create_dir_all(parent)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(parent.join("tasks.lock"))?;
    lock.lock_exclusive()?;

    // Re-check under the lock: a concurrent instance may have finished the
    // migration while we waited.
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let board: legacy::TaskBoard = serde_json::from_str(&raw).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("migrate {}: {}", path.display(), e),
        )
    })?;

    let mut migrated = 0usize;
    let mut all_ids: Vec<String> = Vec::with_capacity(board.tasks.len());
    for item in board.tasks {
        all_ids.push(item.id.clone());
        let exists = personal_task_dir(&item.id).is_some_and(|d| d.exists());
        if exists {
            continue;
        }
        write_task_state(&legacy_item_to_state(item))?;
        migrated += 1;
    }

    // Paranoia gate before the destructive rename: re-verify every task is
    // actually present in the store. Guards against anything that redirects
    // path resolution mid-migration (observed: a parallel test flipping
    // $HOME, sending the writes into a doomed tempdir) — better to leave the
    // trigger armed and error than to disarm it with the data elsewhere.
    for id in &all_ids {
        let landed = personal_task_dir(id).is_some_and(|d| d.join("state.json").exists());
        if !landed {
            return Err(io::Error::other(format!(
                "migration verify failed: task {} missing from the store; \
                 leaving tasks.json in place",
                id
            )));
        }
    }

    if let Some(cwd) = board.last_assign_cwd {
        let mut meta = load_board_meta();
        if meta.last_assign_cwd.is_none() {
            meta.last_assign_cwd = Some(cwd);
            save_board_meta(&meta)?;
        }
    }

    // All writes landed; disarm the trigger, keeping the original as backup.
    fs::rename(&path, parent.join("tasks.json.migrated-v1"))?;
    Ok(Some(migrated))
}

/// Promote a personal-board task into a registered project's Backlog: the
/// same record continues under `~/.cc-hub/projects/<pid>/tasks/<tid>/`, where
/// triage, `task start`, and the Backlog popup pick it up like any
/// orchestrated task. Prompt, tags, priority, created_at, cwd, and
/// session_id travel along (history); `tmux` is cleared so a live board
/// agent stays visible on the Sessions tab rather than being mistaken for an
/// orchestrator. Write-then-delete: a crash between the two leaves a
/// duplicate, never a loss.
pub fn promote_task(task_id: &str, project_id: &str) -> io::Result<TaskState> {
    let projects = orchestrator::load_projects();
    let project = projects
        .projects
        .iter()
        .find(|p| p.id == project_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("project {} is not registered", project_id),
            )
        })?;

    let mut state = read_task_state_for(None, task_id)?;
    if state.project_id.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("task {} already belongs to a project", task_id),
        ));
    }
    state.project_id = Some(project.id.clone());
    state.project_root = Some(project.root.clone());
    // The orchestrated flow starts at Backlog regardless of which board
    // column the card sat in; done_at would contradict Backlog.
    state.status = TaskStatus::Backlog;
    state.done_at = None;
    state.tmux = None;
    state.touch();
    write_task_state(&state)?;

    if let Some(dir) = personal_task_dir(task_id) {
        match fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            // The project copy already landed; a leftover personal dir is a
            // duplicate the user can delete, not data loss.
            Err(e) => log::warn!("promote {}: personal dir cleanup failed: {}", task_id, e),
        }
    }
    Ok(state)
}

// Unix-only for the same reason as todo.rs: isolation works by redirecting
// `$HOME`, which `dirs::home_dir()` ignores on Windows.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_util::with_temp_home;

    #[test]
    fn add_persists_round_trip() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            assert!(b.tasks().is_empty());
            let id = b.add("  fix the flaky test  ").unwrap().unwrap();
            assert_eq!(b.add("   ").unwrap(), None);
            let reloaded = PersonalBoard::load();
            assert_eq!(reloaded.tasks().len(), 1);
            let t = reloaded.get(&id).unwrap();
            assert_eq!(t.prompt, "fix the flaky test");
            assert_eq!(t.status, TaskStatus::Backlog);
            assert_eq!(t.kind(), orchestrator::TaskKind::Personal);
            assert!(t.task_id.starts_with("tk-"));
        });
    }

    #[test]
    fn status_transitions_stamp_done_at() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            let id = b.add("ship it").unwrap().unwrap();
            b.set_status(&id, TaskStatus::Done).unwrap();
            let t = PersonalBoard::load();
            let done = t.get(&id).unwrap();
            assert_eq!(done.status, TaskStatus::Done);
            assert!(done.done_at.is_some());
            b.set_status(&id, TaskStatus::Backlog).unwrap();
            assert!(PersonalBoard::load().get(&id).unwrap().done_at.is_none());
        });
    }

    #[test]
    fn illegal_edge_is_rejected_and_not_persisted() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            let id = b.add("no PR flow here").unwrap().unwrap();
            // Backlog → Review is orchestrated-only; the shared table must
            // refuse it for a personal task.
            let err = b.set_status(&id, TaskStatus::Review).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(
                PersonalBoard::load().get(&id).unwrap().status,
                TaskStatus::Backlog
            );
            // The rejected write must not poison the in-memory snapshot.
            assert_eq!(b.get(&id).unwrap().status, TaskStatus::Backlog);
        });
    }

    #[test]
    fn assign_moves_to_planning_and_rebind_keeps_session() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            let id = b.add("write the parser").unwrap().unwrap();
            b.assign(&id, "/tmp/proj", "claude", "cchub-1-42").unwrap();
            let t = PersonalBoard::load();
            let task = t.get(&id).unwrap();
            assert_eq!(task.status, TaskStatus::Planning);
            assert_eq!(task.cwd.as_deref(), Some("/tmp/proj"));
            assert_eq!(task.tmux.as_deref(), Some("cchub-1-42"));
            assert_eq!(task.session_id, None);
            b.rebind_tmux(&id, "cchub-1-43").unwrap();
            assert_eq!(
                PersonalBoard::load().get(&id).unwrap().tmux.as_deref(),
                Some("cchub-1-43")
            );
        });
    }

    #[test]
    fn assign_records_last_assign_cwd_across_reloads() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            assert_eq!(b.last_assign_cwd(), None);
            let id = b.add("t").unwrap().unwrap();
            b.assign(&id, "/tmp/proj", "claude", "cchub-1-42").unwrap();
            assert_eq!(PersonalBoard::load().last_assign_cwd(), Some("/tmp/proj"));
            // Deleting the task must not forget where it ran.
            b.remove(&id).unwrap();
            assert_eq!(PersonalBoard::load().last_assign_cwd(), Some("/tmp/proj"));
        });
    }

    #[test]
    fn new_tasks_default_priority_and_set_priority_round_trips() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            let id = b.add("triage the backlog").unwrap().unwrap();
            // New tasks start at the default priority (P3).
            assert_eq!(b.get(&id).unwrap().priority, TaskPriority::default());
            b.set_priority(&id, TaskPriority::P1).unwrap();
            assert_eq!(
                PersonalBoard::load().get(&id).unwrap().priority,
                TaskPriority::P1
            );
            // Re-setting the same level is a no-op (and still persisted state
            // is unchanged).
            b.set_priority(&id, TaskPriority::P1).unwrap();
            assert_eq!(b.get(&id).unwrap().priority, TaskPriority::P1);
        });
    }

    #[test]
    fn parse_tags_normalizes_and_caps() {
        // Splits on commas and whitespace, strips '#', lowercases, dedupes.
        assert_eq!(
            parse_tags("#Bug,  api   bug BACKEND"),
            vec!["bug", "api", "backend"]
        );
        assert!(parse_tags("   ").is_empty());
        // Per-tag length cap.
        let long = "a".repeat(40);
        assert_eq!(parse_tags(&long), vec!["a".repeat(MAX_TAG_LEN)]);
        // Set-size cap (seven distinct tags → MAX_TAGS kept).
        let many = "t1 t2 t3 t4 t5 t6 t7";
        assert_eq!(parse_tags(many).len(), MAX_TAGS);
    }

    #[test]
    fn set_tags_round_trips_and_skips_unchanged() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            let id = b.add("label me").unwrap().unwrap();
            assert!(b.get(&id).unwrap().tags.is_empty());
            b.set_tags(&id, parse_tags("bug api")).unwrap();
            assert_eq!(
                PersonalBoard::load().get(&id).unwrap().tags,
                vec!["bug", "api"]
            );
            // Re-setting the same set is a no-op; clearing removes them.
            b.set_tags(&id, parse_tags("bug api")).unwrap();
            assert_eq!(b.get(&id).unwrap().tags, vec!["bug", "api"]);
            b.set_tags(&id, Vec::new()).unwrap();
            assert!(PersonalBoard::load().get(&id).unwrap().tags.is_empty());
        });
    }

    #[test]
    fn priority_orders_p1_before_p4() {
        assert!(TaskPriority::P1 < TaskPriority::P2);
        assert!(TaskPriority::P2 < TaskPriority::P3);
        assert!(TaskPriority::P3 < TaskPriority::P4);
    }

    #[test]
    fn column_filters_by_status() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            let a = b.add("a").unwrap().unwrap();
            b.add("b").unwrap().unwrap();
            b.set_status(&a, TaskStatus::Done).unwrap();
            assert_eq!(b.column(TaskStatus::Backlog).len(), 1);
            assert_eq!(b.column(TaskStatus::Done).len(), 1);
            assert_eq!(b.column(TaskStatus::Planning).len(), 0);
            assert_eq!(b.column(TaskStatus::Running).len(), 0);
        });
    }

    #[test]
    fn clear_done_and_remove() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            let a = b.add("a").unwrap().unwrap();
            let c = b.add("c").unwrap().unwrap();
            b.set_status(&a, TaskStatus::Done).unwrap();
            let cleared = b.clear_done().unwrap();
            assert_eq!(cleared.len(), 1);
            assert_eq!(cleared[0].task_id, a);
            assert!(b.clear_done().unwrap().is_empty());
            assert!(b.remove(&c).unwrap().is_some());
            assert!(b.remove(&c).unwrap().is_none());
            assert!(PersonalBoard::load().tasks().is_empty());
        });
    }

    #[test]
    fn removals_land_in_archive_and_restore_reinserts() {
        with_temp_home(|| {
            let mut b = PersonalBoard::load();
            let a = b.add("deleted one").unwrap().unwrap();
            let d = b.add("done one").unwrap().unwrap();
            b.set_status(&d, TaskStatus::Done).unwrap();
            let removed = b.remove(&a).unwrap().unwrap();
            b.clear_done().unwrap();
            // Both removal paths append to the archive file.
            let raw = fs::read_to_string(archive_path().unwrap()).unwrap();
            let archived: Vec<TaskState> = serde_json::from_str(&raw).unwrap();
            assert_eq!(
                archived
                    .iter()
                    .map(|t| t.task_id.as_str())
                    .collect::<Vec<_>>(),
                vec![a.as_str(), d.as_str()]
            );
            // Undo restores the task (with its id); a second restore of the
            // same id is refused.
            assert!(b.restore(removed.clone()).unwrap());
            assert!(!b.restore(removed).unwrap());
            assert_eq!(PersonalBoard::load().get(&a).unwrap().prompt, "deleted one");
        });
    }

    #[test]
    fn concurrent_instances_merge_at_task_granularity() {
        with_temp_home(|| {
            // Two boards loaded from the same (empty) store: each adds a
            // task; both must land on disk. The old single-file board's CAS
            // would have rejected the second writer entirely.
            let mut first = PersonalBoard::load_result().unwrap();
            let mut second = PersonalBoard::load_result().unwrap();
            first.add("first writer").unwrap().unwrap();
            second.add("second writer").unwrap().unwrap();

            let merged = PersonalBoard::load_result().unwrap();
            assert_eq!(merged.tasks().len(), 2);
        });
    }

    #[test]
    fn malformed_task_file_is_reported_without_replacing_it() {
        with_temp_home(|| {
            let dir = personal_task_dir("tk-broken").unwrap();
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("state.json"), "{not-json").unwrap();

            let error = PersonalBoard::load_result().unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(
                fs::read_to_string(dir.join("state.json")).unwrap(),
                "{not-json"
            );
        });
    }

    #[test]
    fn parse_quick_add_splits_text_tags_priority() {
        let q = parse_quick_add("fix the parser #bug #api !2");
        assert_eq!(q.text, "fix the parser");
        assert_eq!(q.tags, vec!["bug", "api"]);
        assert_eq!(q.priority, Some(TaskPriority::P2));
        // Syntax tokens can sit anywhere; the last `!N` wins; a bare `#`
        // and an out-of-range `!5` stay text.
        let q = parse_quick_add("!4 ship #Infra it !1 # !5");
        assert_eq!(q.text, "ship it # !5");
        assert_eq!(q.tags, vec!["infra"]);
        assert_eq!(q.priority, Some(TaskPriority::P1));
        // Plain input passes through untouched.
        let q = parse_quick_add("just words");
        assert_eq!(q.text, "just words");
        assert!(q.tags.is_empty());
        assert_eq!(q.priority, None);
    }

    // ── migration ─────────────────────────────────────────────────────────

    fn write_legacy_board(json: &str) {
        let path = legacy_tasks_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, json).unwrap();
    }

    const LEGACY_BOARD: &str = r#"{
        "tasks": [
            {"id":"tk-1","text":"old todo","status":"todo","created_at":100},
            {"id":"tk-2","text":"planned","status":"planning","priority":"p1",
             "tags":["bug"],"created_at":200,"cwd":"/tmp/p","agent_id":"claude",
             "tmux":"cchub-1-1","session_id":"sid-1"},
            {"id":"tk-3","text":"working","status":"in_progress","created_at":300},
            {"id":"tk-4","text":"shipped","status":"done","created_at":400,"done_at":450}
        ],
        "last_assign_cwd": "/tmp/p",
        "revision": 9
    }"#;

    #[test]
    fn migration_maps_every_field_and_disarms() {
        with_temp_home(|| {
            write_legacy_board(LEGACY_BOARD);
            assert_eq!(migrate_legacy_board().unwrap(), Some(4));

            let b = PersonalBoard::load_result().unwrap();
            assert_eq!(b.tasks().len(), 4);
            assert_eq!(b.last_assign_cwd(), Some("/tmp/p"));

            let t1 = b.get("tk-1").unwrap();
            assert_eq!(t1.status, TaskStatus::Backlog);
            assert_eq!(t1.prompt, "old todo");
            assert_eq!(t1.created_at, 100);
            assert_eq!(t1.priority, TaskPriority::P3);
            assert!(t1.project_id.is_none());

            let t2 = b.get("tk-2").unwrap();
            assert_eq!(t2.status, TaskStatus::Planning);
            assert_eq!(t2.priority, TaskPriority::P1);
            assert_eq!(t2.tags, vec!["bug"]);
            assert_eq!(t2.cwd.as_deref(), Some("/tmp/p"));
            assert_eq!(t2.session_id.as_deref(), Some("sid-1"));

            assert_eq!(b.get("tk-3").unwrap().status, TaskStatus::Running);

            let t4 = b.get("tk-4").unwrap();
            assert_eq!(t4.status, TaskStatus::Done);
            assert_eq!(t4.done_at, Some(450));
            assert_eq!(t4.updated_at, 450);

            // The trigger is disarmed and the backup preserved.
            assert!(!legacy_tasks_path().unwrap().exists());
            let backup = legacy_tasks_path()
                .unwrap()
                .parent()
                .unwrap()
                .join("tasks.json.migrated-v1");
            assert!(backup.exists());

            // Re-running is a no-op.
            assert_eq!(migrate_legacy_board().unwrap(), None);
        });
    }

    #[test]
    fn migration_skips_existing_dirs_on_rerun() {
        with_temp_home(|| {
            write_legacy_board(LEGACY_BOARD);
            // Simulate a partial earlier run: tk-1 already migrated (with an
            // edit the re-run must not clobber).
            let mut pre = TaskState::new_personal("edited after partial run".into());
            pre.task_id = "tk-1".into();
            write_task_state(&pre).unwrap();

            assert_eq!(migrate_legacy_board().unwrap(), Some(3));
            let b = PersonalBoard::load_result().unwrap();
            assert_eq!(b.tasks().len(), 4);
            assert_eq!(b.get("tk-1").unwrap().prompt, "edited after partial run");
        });
    }

    #[test]
    fn corrupt_legacy_board_aborts_untouched() {
        with_temp_home(|| {
            write_legacy_board("{not-json");
            let error = migrate_legacy_board().unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            // Nothing migrated, file untouched, trigger still armed.
            assert!(PersonalBoard::load_result().unwrap().tasks().is_empty());
            assert_eq!(
                fs::read_to_string(legacy_tasks_path().unwrap()).unwrap(),
                "{not-json"
            );
        });
    }

    #[test]
    fn promote_moves_task_into_project_backlog() {
        with_temp_home(|| {
            let root = std::env::temp_dir().join("promote-fixture");
            fs::create_dir_all(&root).unwrap();
            let project_id =
                orchestrator::ensure_project_registered(&root, "promote-fixture").unwrap();

            let mut b = PersonalBoard::load();
            let id = b.add("grow into a project task").unwrap().unwrap();
            b.assign(&id, "/tmp/p", "claude", "cchub-1-9").unwrap();

            let promoted = promote_task(&id, &project_id).unwrap();
            assert_eq!(promoted.project_id.as_deref(), Some(project_id.as_str()));
            assert_eq!(promoted.status, TaskStatus::Backlog);
            assert_eq!(promoted.tmux, None, "board tmux must not travel");
            assert_eq!(promoted.cwd.as_deref(), Some("/tmp/p"), "history travels");

            // Off the board, present in the project store.
            assert!(PersonalBoard::load().get(&id).is_none());
            let in_project = orchestrator::read_task_state(&project_id, &id).unwrap();
            assert_eq!(in_project.prompt, "grow into a project task");

            // Unknown project and double-promotion are refused.
            assert!(promote_task(&id, "nope").is_err());
        });
    }

    #[test]
    fn migrated_task_round_trips_through_the_store() {
        with_temp_home(|| {
            write_legacy_board(LEGACY_BOARD);
            migrate_legacy_board().unwrap();
            // A migrated Planning card approves to Running like a native one.
            let mut b = PersonalBoard::load_result().unwrap();
            b.set_status("tk-2", TaskStatus::Running).unwrap();
            assert_eq!(
                PersonalBoard::load().get("tk-2").unwrap().status,
                TaskStatus::Running
            );
        });
    }
}
