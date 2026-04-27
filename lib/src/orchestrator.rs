//! Strategic / Projects layer.
//!
//! cc-hub today owns Claude Code sessions one-by-one. The orchestrator layer
//! sits one level higher: a **project** is a registered directory (usually a
//! git repo root); a **task** is a high-level user request handled by a
//! single **orchestrator session** which spawns and manages worker sessions.
//!
//! State for each task lives at
//! `~/.cc-hub/projects/<project-id>/tasks/<task-id>/state.json` and is
//! written by the orchestrator (via `cc-hub task report`, `cc-hub
//! spawn-worker`, `cc-hub merge-worktree`) and read by the TUI's Projects
//! view. The list of registered projects lives at `~/.cc-hub/projects.toml`.
//!
//! This module owns only the schema + on-disk helpers. The CLI subcommands
//! that mutate it live in `bin/src/cli.rs`; the TUI consumer lives in
//! `lib/src/ui.rs`.
//!
//! Project ID derivation: canonical path with non-alphanumeric runs collapsed
//! to single dashes. Stable, human-readable, no hashing dep needed. Two
//! different paths can in theory collide (e.g. `/foo/bar` and `/foo-bar`),
//! but in practice every project is a real filesystem path so collisions
//! require deliberate construction.
//!
//! Task ID format: `t-<unix-nanos>`. Sortable, unique within a single host
//! to nanosecond resolution, no extra dep.
//!
//! Worktree convention: `<project-root>/.cc-hub-wt/<task-id>-<name>` off
//! `main`. The orchestrator picks `<name>`; cc-hub creates the directory and
//! the branch.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Root of cc-hub's user state. `~/.cc-hub/`. None when home is unresolvable.
pub fn cc_hub_home() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cc-hub"))
}

pub fn projects_toml_path() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("projects.toml"))
}

pub fn projects_state_dir() -> Option<PathBuf> {
    cc_hub_home().map(|h| h.join("projects"))
}

pub fn project_state_dir(project_id: &str) -> Option<PathBuf> {
    projects_state_dir().map(|d| d.join(project_id))
}

pub fn task_state_dir(project_id: &str, task_id: &str) -> Option<PathBuf> {
    project_state_dir(project_id).map(|d| d.join("tasks").join(task_id))
}

pub fn task_state_file(project_id: &str, task_id: &str) -> Option<PathBuf> {
    task_state_dir(project_id, task_id).map(|d| d.join("state.json"))
}

pub fn task_orchestrator_log_path(project_id: &str, task_id: &str) -> Option<PathBuf> {
    task_state_dir(project_id, task_id).map(|d| d.join("orchestrator.log"))
}

/// Compute a stable, human-readable project id from a filesystem path. The
/// path is canonicalised when possible; symlink targets normalise to the same
/// id as the symlink itself.
pub fn project_id_for_path(root: &Path) -> String {
    let canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let raw = canon.to_string_lossy();
    let mut id = String::with_capacity(raw.len());
    let mut last_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            id.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            id.push('-');
            last_dash = true;
        }
    }
    let trimmed = id.trim_matches('-');
    if trimmed.is_empty() {
        "root".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn new_task_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("t-{}", nanos)
}

/// Compact rendering of a `t-<unix-nanos>` id for in-card display. Last 6
/// digits are unique within the active set without dominating the badge.
pub fn short_task_id(task_id: &str) -> String {
    let trimmed = task_id.trim_start_matches("t-");
    let take = trimmed.len().saturating_sub(6);
    trimmed[take..].to_string()
}

pub fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worker {
    pub tmux_name: String,
    pub cwd: PathBuf,
    pub worktree: Option<String>,
    pub readonly: bool,
    pub spawned_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MergeOutcome {
    Ok,
    Conflict { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeRecord {
    pub worktree: String,
    pub at: i64,
    pub outcome: MergeOutcome,
}

/// A piece of evidence the orchestrator (or a worker) attached to a task —
/// screenshot, log, build output, URL, etc. Stored alongside the task state
/// so it survives worktree cleanup. `kind` is free-form by design; the CLI
/// suggests common values but doesn't constrain them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub kind: String,
    /// Absolute path to the copied-into-state file, OR the original URL when
    /// `kind == "url"` (or any URL-shaped path).
    pub path: String,
    /// User-supplied path/url, preserved so consumers can show where the
    /// artifact originated even after cc-hub has copied it into its store.
    pub original: String,
    pub caption: Option<String>,
    pub added_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskState {
    pub task_id: String,
    pub project_id: String,
    pub project_root: PathBuf,
    /// Filled in by the orchestrator the first time it reports — cc-hub
    /// can't know its session id at spawn time (Claude generates it).
    #[serde(default)]
    pub orchestrator_session_id: Option<String>,
    /// tmux session name hosting the orchestrator. Set by `orchestrate
    /// start` immediately after spawn so the TUI / scanner can group
    /// child workers under the right parent without waiting for the
    /// orchestrator to self-report.
    #[serde(default)]
    pub orchestrator_tmux: Option<String>,
    pub status: TaskStatus,
    /// Free-form prompt the user submitted when creating the task. Frozen
    /// after creation; the orchestrator sees it via its system prompt.
    pub prompt: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// One-line latest status from the orchestrator. Surface in the
    /// Projects view so the user can skim a project at a glance.
    #[serde(default)]
    pub note: Option<String>,
    /// Multi-line proof-of-work summary written by the orchestrator on
    /// completion. Distinct from `note`, which is the latest one-line
    /// status. `serde(default)` so older state.json files still load.
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub workers: Vec<Worker>,
    #[serde(default)]
    pub merges: Vec<MergeRecord>,
    /// Proof-of-work artifacts attached over the task's lifetime. Append
    /// only via the CLI; `serde(default)` for back-compat.
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
}

impl TaskState {
    pub fn new(project_id: String, project_root: PathBuf, prompt: String) -> Self {
        let now = now_unix_secs();
        Self {
            task_id: new_task_id(),
            project_id,
            project_root,
            orchestrator_session_id: None,
            orchestrator_tmux: None,
            status: TaskStatus::Running,
            prompt,
            created_at: now,
            updated_at: now,
            note: None,
            summary: None,
            workers: Vec::new(),
            merges: Vec::new(),
            artifacts: Vec::new(),
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = now_unix_secs();
    }
}

/// Read a task state file; missing file returns NotFound, parse errors
/// surface as InvalidData so callers can distinguish "no such task" from
/// "schema drift".
pub fn read_task_state(project_id: &str, task_id: &str) -> io::Result<TaskState> {
    let path = task_state_file(project_id, task_id)
        .ok_or_else(|| io::Error::other("no home dir"))?;
    let raw = fs::read_to_string(&path)?;
    serde_json::from_str(&raw)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {}", path.display(), e)))
}

/// Atomically write a task state file via tempfile + rename. Creates parent
/// dirs on demand.
pub fn write_task_state(state: &TaskState) -> io::Result<()> {
    let path = task_state_file(&state.project_id, &state.task_id)
        .ok_or_else(|| io::Error::other("no home dir"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(state)
        .map_err(|e| io::Error::other(format!("serialize state: {}", e)))?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        use std::io::Write;
        let mut f = fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// In-place update under `read → mutate → write`. The closure receives a
/// mutable state and is responsible for any field-level changes; `touch()`
/// is called automatically after the closure so callers don't have to
/// remember.
pub fn update_task_state<F>(project_id: &str, task_id: &str, f: F) -> io::Result<TaskState>
where
    F: FnOnce(&mut TaskState),
{
    let mut state = read_task_state(project_id, task_id)?;
    f(&mut state);
    state.touch();
    write_task_state(&state)?;
    Ok(state)
}

/// One registered project. Stored in `~/.cc-hub/projects.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectsFile {
    #[serde(default, rename = "project")]
    pub projects: Vec<Project>,
}

pub fn load_projects() -> ProjectsFile {
    let Some(path) = projects_toml_path() else {
        return ProjectsFile::default();
    };
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return ProjectsFile::default(),
        Err(e) => {
            log::warn!("projects.toml: read error at {}: {}", path.display(), e);
            return ProjectsFile::default();
        }
    };
    match toml::from_str::<ProjectsFile>(&raw) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("projects.toml: parse error at {}: {}", path.display(), e);
            ProjectsFile::default()
        }
    }
}

pub fn save_projects(file: &ProjectsFile) -> io::Result<()> {
    let path = projects_toml_path().ok_or_else(|| io::Error::other("no home dir"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = toml::to_string_pretty(file)
        .map_err(|e| io::Error::other(format!("serialize projects.toml: {}", e)))?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        use std::io::Write;
        let mut f = fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Register `root` if it isn't already, returning the project id either
/// way. `name` is used only when inserting a new entry.
pub fn ensure_project_registered(root: &Path, name: &str) -> io::Result<String> {
    let id = project_id_for_path(root);
    let mut file = load_projects();
    if !file.projects.iter().any(|p| p.id == id) {
        let canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        file.projects.push(Project {
            id: id.clone(),
            name: name.to_string(),
            root: canon,
            created_at: now_unix_secs(),
        });
        save_projects(&file)?;
    }
    Ok(id)
}

/// First user message dispatched to a freshly-spawned orchestrator session.
///
/// This is *the* contract between cc-hub and the orchestrator role: it
/// teaches a stock Claude Code session about the four CLI primitives, sets
/// expectations (decompose, don't impl; parallelize reads; serialize edits),
/// and embeds the user's actual task verbatim. Keep it concise — the
/// orchestrator pays for it on every turn.
///
/// `cc_hub_bin` is the absolute path to the cc-hub binary running this
/// process — pre-substituted into every example so the orchestrator's Bash
/// shell doesn't need cc-hub on `PATH` (a real failure mode observed in
/// the first end-to-end run, where the orch had to guess the path).
pub fn build_orchestrator_prompt(state: &TaskState, cc_hub_bin: &Path) -> String {
    let TaskState {
        task_id,
        project_id,
        project_root,
        prompt,
        ..
    } = state;
    let bin = cc_hub_bin.display();
    format!(
        "You are the cc-hub orchestrator for task `{task_id}` in project `{project_id}` at `{root}`.

Your job is to decompose, dispatch, monitor, and merge — not to write code yourself. Run cc-hub subcommands via the Bash tool to spawn worker sessions and track progress. Keep your own session focused on coordination.

The cc-hub binary is at `{bin}`. Always invoke it by absolute path; it is not necessarily on PATH inside worker shells.

# How to work

1. **Plan.** Break the task into sub-tasks. Note which can run in parallel (read-only, or edits to disjoint files) and which must run serially.

2. **Dispatch workers.** Two modes:
   - `{bin} spawn-worker --task {task_id} --readonly --prompt \"…\"` — read/research tasks. No edits, no worktree, runs in the project root. Many can run at once.
   - `{bin} spawn-worker --task {task_id} --worktree NAME --prompt \"…\"` — editing tasks. cc-hub creates a fresh worktree at `.cc-hub-wt/{task_id}-NAME` on a new branch off the project's main branch. Two worktree workers can run in parallel **only** if they edit disjoint sets of files; otherwise serialize them.
   Each spawn-worker emits one JSON line on stdout — capture the `tmux` field if you need to interact with that worker later.

3. **Monitor.** Two reliable channels, in order of preference:
   - `tmux capture-pane -t <tmux>:0 -p | tail -40` — shows the worker's current screen, including its last action and whether it's at the input prompt (idle) or thinking. Use this to tell when a worker is done.
   - JSONL transcripts under `~/.claude/projects/<sanitised-cwd>/<sid>.jsonl` — full conversation history, useful when you need detail.
   Avoid `until [ -f X ]; do sleep …; done` shell loops on file existence — they hide a stuck worker behind a 5-minute timeout.

4. **Merge edits.** When a worktree worker finishes:
   `{bin} merge-worktree --task {task_id} --worktree NAME`
   On success, the branch is in the project's main branch and the worktree dir stays for inspection. On conflict, the JSON output describes what failed; either resolve manually with git in the worktree, or spawn a follow-up worker to resolve it.

5. **Report progress** after each meaningful step (worker spawned, worker finished, merge attempted, plan changed):
   `{bin} task report --task {task_id} --status running --note \"<one line>\"`
   These notes surface in the user's Projects view. Keep them terse — milestones, not play-by-play.

6. **Finish.** When the user's task is complete, gather proof of work (see next section) and then:
   `{bin} task report --task {task_id} --status done --note \"<one line>\" --summary \"<multi-line briefing>\"`
   On unrecoverable failure: `--status failed --note \"<why>\"`.

# Proof of work

Done without proof is not done. Before you mark a task done, gather concrete evidence that the change works and attach it to the task — the user reads this to verify the outcome without re-running everything you did.

Two primitives:
- `{bin} task artifact add --task {task_id} --path PATH [--kind KIND] [--caption TEXT]` — attach a file (copied into cc-hub's store, survives worktree cleanup) or a URL (stored as-is). `KIND` is free-form; common values: `screenshot`, `video`, `log`, `build`, `test`, `diff`, `file`, `url`. URL-shaped paths default to `kind=url`.
- `{bin} task artifact list --task {task_id}` — review what's already attached.

What counts as proof, by change type:
- **Web / UI** — screenshot of the rendered feature. For regression fixes, attach before *and* after.
- **CLI / library / backend** — terminal recording (asciinema if available), or capture the relevant command output to a file and attach it with `--kind log`.
- **Tests / CI / build** — the build log file, or a URL to the CI run with `--kind url`.
- **Refactors (no behavioural change)** — a `diff` artifact summarising the change plus a `log` artifact showing build + tests still pass.
- **Bug fixes** — a `log` showing the repro failing before and passing after the fix, OR a regression test added in the same change (mention its path in the summary).

On the final `task report --status done` call, pass a multi-line `--summary` covering:
1. what was done (one paragraph),
2. what you verified (point at the artifacts),
3. key files changed (a few — not exhaustive),
4. what was deliberately out of scope.

The summary is the one-screen briefing the user reads to understand the task's outcome. Use a heredoc so newlines survive the shell:
`{bin} task report --task {task_id} --status done --note \"<one line>\" --summary \"$(cat <<'EOF'
<multi-line summary>
EOF
)\"`

# Rules

- Don't ask the user clarifying questions. If the task is ambiguous, pick the most reasonable interpretation and note your assumption in the first status report.
- Don't implement yourself unless the task is genuinely tiny (a few lines in one file) and faster to do than to dispatch.
- Each worktree owns its files. Don't run two parallel worktree workers whose files overlap.
- If a worker creates or edits `.gitignore`, instruct it to include `.cc-hub-wt/` so cc-hub's worktree dirs don't pollute future commits.
- The user can micro-manage from the Sessions view; you don't need to surface every detail in reports — just milestones.

# Your task

{prompt}

Begin by writing your decomposition plan as the first `{bin} task report` call, then start dispatching.",
        task_id = task_id,
        project_id = project_id,
        root = project_root.display(),
        bin = bin,
        prompt = prompt,
    )
}

/// Create + persist a fresh task and spawn its orchestrator session.
///
/// Returns the `(TaskState, tmux_session_name)` so callers can immediately
/// queue the orchestrator prompt for dispatch (typically via the pending-
/// dispatch path: spawn now, send when Idle). The system prompt is built
/// against `cc_hub_bin`, which the caller resolves with
/// [`std::env::current_exe`].
///
/// Concretely:
/// 1. registers the project (if new) in `~/.cc-hub/projects.toml`
/// 2. writes the initial task state
/// 3. spawns `cc-hub-new` via the existing detached-tmux pathway
/// 4. records the resulting tmux name back on the state
///
/// This mirrors what `cc-hub orchestrate start` does, minus the synchronous
/// idle-poll/dispatch — the TUI prefers async dispatch so the keystroke
/// returns instantly.
pub fn spawn_orchestrator_for_new_task(
    project_root: &Path,
    project_name: &str,
    user_prompt: String,
) -> io::Result<(TaskState, String, String)> {
    let project_id = ensure_project_registered(project_root, project_name)?;
    let canonical_root = fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let mut state = TaskState::new(project_id, canonical_root.clone(), user_prompt);
    write_task_state(&state)?;

    let cwd = canonical_root.to_string_lossy().into_owned();
    let tmux_name = crate::spawn::spawn_claude_session(&cwd, None)?;

    state.orchestrator_tmux = Some(tmux_name.clone());
    state.touch();
    write_task_state(&state)?;

    let cc_hub_bin = std::env::current_exe()
        .map_err(|e| io::Error::other(format!("resolve cc-hub binary path: {}", e)))?;
    let orchestrator_prompt = build_orchestrator_prompt(&state, &cc_hub_bin);

    Ok((state, tmux_name, orchestrator_prompt))
}

/// Standard worktree path for `<task>-<name>` under `<root>/.cc-hub-wt/`.
pub fn worktree_path(project_root: &Path, task_id: &str, name: &str) -> PathBuf {
    project_root
        .join(".cc-hub-wt")
        .join(format!("{}-{}", task_id, name))
}

/// Branch name for a worktree. Mirrors the dir name so `git worktree list`
/// is readable.
pub fn worktree_branch(task_id: &str, name: &str) -> String {
    format!("cc-hub/{}-{}", task_id, name)
}

/// Detect the project's primary branch. Tries `origin/HEAD`, then `main`,
/// then `master`, falling back to `"main"` (which lets the caller's git
/// command surface the real failure rather than us inventing one).
pub fn detect_main_branch(root: &Path) -> String {
    if let Ok(out) = run_git(root, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]) {
        if out.status_ok {
            if let Some(name) = out.stdout.trim().strip_prefix("origin/") {
                return name.to_string();
            }
        }
    }
    for candidate in ["main", "master"] {
        let exists = run_git(root, &["rev-parse", "--verify", "--quiet", candidate])
            .map(|o| o.status_ok)
            .unwrap_or(false);
        if exists {
            return candidate.to_string();
        }
    }
    "main".to_string()
}

/// Result of running `git -C <root> <args>`. Distinguishes "non-zero exit"
/// (error in the command) from "couldn't even invoke git" (env problem).
pub struct GitOutput {
    pub status_ok: bool,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_git(root: &Path, args: &[&str]) -> io::Result<GitOutput> {
    let out = Command::new("git").arg("-C").arg(root).args(args).output()?;
    Ok(GitOutput {
        status_ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Create a worktree at `<root>/.cc-hub-wt/<task>-<name>` on a fresh branch
/// `cc-hub/<task>-<name>` based on `<base>`. Idempotent: a pre-existing
/// worktree is detected from git's stderr and reused so a re-running
/// orchestrator doesn't trip on its own previous attempt.
pub fn create_worktree(
    project_root: &Path,
    task_id: &str,
    name: &str,
    base_branch: &str,
) -> io::Result<PathBuf> {
    let path = worktree_path(project_root, task_id, name);
    let branch = worktree_branch(task_id, name);
    let out = run_git(
        project_root,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            &path.to_string_lossy(),
            base_branch,
        ],
    )?;
    if out.status_ok {
        return Ok(path);
    }
    // git's "already exists" / "is already checked out" messages mean a
    // previous run left this worktree behind; reuse rather than fail.
    let stderr = out.stderr.trim();
    let already = stderr.contains("already exists") || stderr.contains("already checked out");
    if already {
        log::info!("create_worktree: {} already exists, reusing", path.display());
        return Ok(path);
    }
    Err(io::Error::other(format!(
        "git worktree add failed: {}",
        stderr
    )))
}

/// Merge `<branch>` into `<main>` from the project root. Returns
/// [`MergeOutcome::Conflict`] (with git's stderr/stdout joined) on a
/// non-zero exit so the caller can persist the failure and surface it to
/// the orchestrator without crashing the CLI.
pub fn merge_branch(
    project_root: &Path,
    main_branch: &str,
    feature_branch: &str,
) -> io::Result<(MergeOutcome, String, String)> {
    let checkout = run_git(project_root, &["checkout", main_branch])?;
    if !checkout.status_ok {
        return Err(io::Error::other(format!(
            "git checkout {} failed: {}",
            main_branch,
            checkout.stderr.trim()
        )));
    }
    let msg = format!("cc-hub: merge {} into {}", feature_branch, main_branch);
    let out = run_git(
        project_root,
        &["merge", "--no-ff", "-m", &msg, feature_branch],
    )?;
    let outcome = if out.status_ok {
        MergeOutcome::Ok
    } else {
        let detail = if !out.stderr.trim().is_empty() {
            out.stderr.clone()
        } else {
            out.stdout.clone()
        };
        MergeOutcome::Conflict {
            detail: detail.trim().to_string(),
        }
    };
    Ok((outcome, out.stdout, out.stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_id_is_stable_and_sanitised() {
        let a = project_id_for_path(Path::new("/home/j-light/git/self/cc-hub"));
        assert_eq!(a, "home-j-light-git-self-cc-hub");

        // collapses runs of separators
        let b = project_id_for_path(Path::new("/foo//bar/_baz_"));
        assert_eq!(b, "foo-bar-baz");

        // empty-ish input falls back
        let c = project_id_for_path(Path::new("/"));
        assert_eq!(c, "root");
    }

    #[test]
    fn task_state_round_trips_through_serde() {
        let mut s = TaskState::new(
            "myproj".into(),
            PathBuf::from("/tmp/myproj"),
            "do the thing".into(),
        );
        s.note = Some("kicked off worker A".into());
        s.workers.push(Worker {
            tmux_name: "cchub-1".into(),
            cwd: PathBuf::from("/tmp/myproj"),
            worktree: Some("a".into()),
            readonly: false,
            spawned_at: 42,
        });
        s.merges.push(MergeRecord {
            worktree: "a".into(),
            at: 99,
            outcome: MergeOutcome::Conflict {
                detail: "conflict in foo.rs".into(),
            },
        });

        let body = serde_json::to_string(&s).unwrap();
        let back: TaskState = serde_json::from_str(&body).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn artifact_round_trips_through_serde() {
        let a = Artifact {
            kind: "screenshot".into(),
            path: "/abs/path/123-foo.png".into(),
            original: "./foo.png".into(),
            caption: Some("login screen after fix".into()),
            added_at: 1_700_000_000,
        };
        let body = serde_json::to_string(&a).unwrap();
        let back: Artifact = serde_json::from_str(&body).unwrap();
        assert_eq!(back, a);

        // No-caption variant — Option<String>::None must serialise+deserialise
        // without surprising the rest of the schema.
        let b = Artifact {
            kind: "url".into(),
            path: "https://example.com/build/42".into(),
            original: "https://example.com/build/42".into(),
            caption: None,
            added_at: 1_700_000_001,
        };
        let body = serde_json::to_string(&b).unwrap();
        let back: Artifact = serde_json::from_str(&body).unwrap();
        assert_eq!(back, b);
    }

    #[test]
    fn task_state_with_artifacts_and_summary_round_trips() {
        let mut s = TaskState::new(
            "myproj".into(),
            PathBuf::from("/tmp/myproj"),
            "do the thing".into(),
        );
        s.summary = Some("shipped feature X.\n\nverified Y, Z.".into());
        s.artifacts.push(Artifact {
            kind: "screenshot".into(),
            path: "/store/123-shot.png".into(),
            original: "shot.png".into(),
            caption: Some("after".into()),
            added_at: 7,
        });
        s.artifacts.push(Artifact {
            kind: "url".into(),
            path: "https://ci.example/run/9".into(),
            original: "https://ci.example/run/9".into(),
            caption: None,
            added_at: 8,
        });

        let body = serde_json::to_string(&s).unwrap();
        let back: TaskState = serde_json::from_str(&body).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn task_state_back_compat_when_artifacts_and_summary_missing() {
        // Mirrors a state.json written by an older cc-hub: no `summary`
        // key, no `artifacts` key. Must still parse, with both fields
        // defaulting empty.
        let raw = r#"{
            "task_id": "t-1",
            "project_id": "p",
            "project_root": "/tmp/p",
            "status": "running",
            "prompt": "hi",
            "created_at": 1,
            "updated_at": 2
        }"#;
        let s: TaskState = serde_json::from_str(raw).unwrap();
        assert_eq!(s.summary, None);
        assert!(s.artifacts.is_empty());
    }

    #[test]
    fn task_status_serialises_lowercase() {
        let s = serde_json::to_string(&TaskStatus::Running).unwrap();
        assert_eq!(s, "\"running\"");
        let f = serde_json::to_string(&TaskStatus::Failed).unwrap();
        assert_eq!(f, "\"failed\"");
    }

    #[test]
    fn worktree_path_includes_task_id() {
        let p = worktree_path(Path::new("/repo"), "t-123", "edit");
        assert_eq!(p, PathBuf::from("/repo/.cc-hub-wt/t-123-edit"));
    }

    #[test]
    fn task_id_format() {
        let id = new_task_id();
        assert!(id.starts_with("t-"));
        assert!(id.len() > 4);
    }

    #[test]
    fn orchestrator_prompt_substitutes_ids_and_user_prompt() {
        let state = TaskState::new(
            "myproj-42".into(),
            PathBuf::from("/work/myproj"),
            "redo the import pipeline so it streams".into(),
        );
        let bin = Path::new("/opt/cc-hub/bin/cc-hub");
        let p = build_orchestrator_prompt(&state, bin);

        // Identity substitutions.
        assert!(p.contains(&state.task_id), "missing task_id");
        assert!(p.contains("myproj-42"), "missing project_id");
        assert!(p.contains("/work/myproj"), "missing project_root");
        assert!(
            p.contains("redo the import pipeline so it streams"),
            "user prompt missing"
        );

        // Binary path appears in every primitive.
        let bin_s = bin.display().to_string();
        let expected_primitives = [
            format!(
                "{} spawn-worker --task {} --readonly",
                bin_s, state.task_id
            ),
            format!(
                "{} spawn-worker --task {} --worktree",
                bin_s, state.task_id
            ),
            format!(
                "{} merge-worktree --task {} --worktree",
                bin_s, state.task_id
            ),
            format!("{} task report --task {}", bin_s, state.task_id),
        ];
        for cmd in &expected_primitives {
            assert!(p.contains(cmd), "primitive missing from prompt: {}", cmd);
        }

        // Load-bearing rules — keep these concise checks so wording can drift.
        assert!(p.contains("decompose"), "missing decomposition framing");
        assert!(
            p.contains("clarifying"),
            "missing 'don't ask clarifying questions' rule"
        );
        // Worktree-gitignore guidance — caught the one shortcoming from
        // the first end-to-end run.
        assert!(
            p.contains(".cc-hub-wt/"),
            "missing .cc-hub-wt/ gitignore guidance"
        );
        // Monitor channel guidance — replaces the 5-min `until` loop
        // pattern that the orchestrator picked up first time.
        assert!(
            p.contains("tmux capture-pane"),
            "missing capture-pane monitor guidance"
        );

        // Proof-of-work guidance — done isn't done without evidence.
        assert!(
            p.contains("Proof of work"),
            "missing proof-of-work section header"
        );
        assert!(
            p.contains(&format!(
                "{} task artifact add --task {}",
                bin_s, state.task_id
            )),
            "missing artifact-add primitive in proof-of-work section"
        );
        assert!(
            p.contains("--summary"),
            "missing --summary guidance for final report"
        );
    }
}
