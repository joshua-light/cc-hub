//! Scanner for the Projects tab.
//!
//! Reads `~/.cc-hub/projects.toml` for the registered project list, then
//! walks `~/.cc-hub/projects/<project-id>/tasks/<task-id>/state.json` for
//! every task. Returns a flat snapshot the TUI can render directly without
//! further IO.
//!
//! Cheap enough to call on each fs-watcher tick: every state.json is still
//! `stat()`-ed each scan so deletions and edits surface immediately, but
//! read+parse is skipped when the cached entry's mtime matches — at
//! ~10 projects × ~30 tasks the saved IO and JSON parsing dominates.
//! Stale cache entries (paths not visited this scan) are evicted in
//! `scan()` so deleted tasks don't linger.

use crate::agent::AgentKind;
use crate::merge_lock::{self, MergeLock};
use crate::orchestrator::{self, Project, TaskState, TaskStatus};
use crate::pr::ReviewState;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

type TaskStateCache = HashMap<PathBuf, (SystemTime, Arc<TaskState>)>;

/// Compact PR summary surfaced on kanban cards. Kept tiny so the
/// snapshot can carry one per task without inflating clone cost: the
/// renderer only needs id + review state + comment count to draw the
/// badge.
#[derive(Debug, Clone, Copy)]
pub struct PrCardSummary {
    pub id: u32,
    pub review_state: ReviewState,
    pub comments: u16,
}

type PrSummaryCache = HashMap<PathBuf, (SystemTime, PrCardSummary)>;

/// Process-global mtime-keyed cache of parsed state.json files. Keyed by
/// the absolute path of `state.json`; value is `(mtime, parsed)`. A scan
/// stat()s every file each tick and only re-reads on mtime change. The
/// parsed value is `Arc`-wrapped so cache hits hand out a cheap clone
/// instead of copying the whole TaskState (Vec<Worker>, prompt strings,
/// merge/artifact lists) per scan.
fn cache() -> &'static Mutex<TaskStateCache> {
    static CACHE: OnceLock<Mutex<TaskStateCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Mirrors [`cache`] for `pr.json` files. Same mtime-keyed read-skip
/// pattern: every scan stat()s each `pr.json` so deletions/edits surface
/// immediately, but parse is skipped when mtime matches.
fn pr_cache() -> &'static Mutex<PrSummaryCache> {
    static CACHE: OnceLock<Mutex<PrSummaryCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Where a tmux session sits in the orchestrator hierarchy. Computed by
/// matching the session's tmux name against every TaskState in the latest
/// snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionRole {
    /// This session is itself an orchestrator for `task_id`.
    Orchestrator {
        task_id: String,
        project_id: String,
        agent_id: String,
        agent_kind: AgentKind,
    },
    /// This session is a worker spawned by the orchestrator for `task_id`.
    Worker {
        task_id: String,
        project_id: String,
        agent_id: String,
        agent_kind: AgentKind,
        worktree: Option<String>,
        readonly: bool,
    },
}

#[derive(Debug, Clone)]
pub struct ProjectsSnapshot {
    pub projects: Vec<Project>,
    /// Tasks for each project_id, sorted newest-first by `updated_at` so
    /// the UI shows the most recently active task at the top.
    pub tasks: HashMap<String, Vec<Arc<TaskState>>>,
    /// Task ids whose Haiku titler is in flight right now. Populated in
    /// the main loop before the snapshot is handed to the App so the UI
    /// can render a spinner without locking a shared mutex per frame.
    pub titling: HashSet<String>,
    /// Current merge-lock holder per project_id. Populated each scan tick so
    /// the renderer can gray out the border of Merging-column cards that are
    /// queued behind another task's in-flight merge. `None` means no live
    /// holder for that project. Stale-lock detection is left to acquire(); the
    /// renderer treats whatever `current_holder` returns as the truth.
    pub merge_lock_holders: HashMap<String, Option<MergeLock>>,
    /// PR id of each merge-lock holder, when one exists. Parallel to
    /// `merge_lock_holders` keyed by project_id. Used by the renderer to
    /// show 'PR #N in /<phase> (<age>)' on queued cards.
    pub merge_lock_holder_pr_ids: HashMap<String, Option<u32>>,
    /// Compact PR summary per `task_id`, populated each scan from the
    /// task's `pr.json`. Empty entry means the task has no PR yet.
    pub pr_summaries: HashMap<String, PrCardSummary>,
}

impl ProjectsSnapshot {
    pub fn empty() -> Self {
        Self {
            projects: Vec::new(),
            tasks: HashMap::new(),
            titling: HashSet::new(),
            merge_lock_holders: HashMap::new(),
            merge_lock_holder_pr_ids: HashMap::new(),
            pr_summaries: HashMap::new(),
        }
    }

    pub fn is_titling(&self, task_id: &str) -> bool {
        self.titling.contains(task_id)
    }

    pub fn active_task_count(&self, project_id: &str) -> usize {
        self.tasks
            .get(project_id)
            .map(|v| v.iter().filter(|t| t.status == TaskStatus::Running).count())
            .unwrap_or(0)
    }

    /// Build a `tmux_name → SessionRole` map for the whole snapshot. The
    /// renderer needs this for every visible card; building one map per
    /// frame keeps the lookup O(1) per card instead of scanning every task
    /// for every card. First-write-wins; orchestrator and worker can't
    /// share a tmux name in practice because each `cc-hub spawn-worker`
    /// allocates a fresh one.
    pub fn roles_by_tmux(&self) -> HashMap<String, SessionRole> {
        let mut out = HashMap::new();
        for tasks in self.tasks.values() {
            for t in tasks {
                if let Some(name) = t.orchestrator_tmux.as_deref() {
                    out.entry(name.to_string())
                        .or_insert(SessionRole::Orchestrator {
                            task_id: t.task_id.clone(),
                            project_id: t.project_id.clone(),
                            agent_id: t.orchestrator_agent_id.clone(),
                            agent_kind: t.orchestrator_agent_kind,
                        });
                }
                for w in &t.workers {
                    out.entry(w.tmux_name.clone())
                        .or_insert(SessionRole::Worker {
                            task_id: t.task_id.clone(),
                            project_id: t.project_id.clone(),
                            agent_id: w.agent_id.clone(),
                            agent_kind: w.agent_kind,
                            worktree: w.worktree.clone(),
                            readonly: w.readonly,
                        });
                }
            }
        }
        out
    }
}

pub fn scan() -> ProjectsSnapshot {
    let projects = orchestrator::load_projects().projects;
    let mut tasks: HashMap<String, Vec<Arc<TaskState>>> = HashMap::new();
    let mut merge_lock_holders: HashMap<String, Option<MergeLock>> = HashMap::new();
    let mut merge_lock_holder_pr_ids: HashMap<String, Option<u32>> = HashMap::new();
    let mut pr_summaries: HashMap<String, PrCardSummary> = HashMap::new();
    let mut visited_all: HashSet<PathBuf> = HashSet::new();
    let mut visited_prs: HashSet<PathBuf> = HashSet::new();

    for p in &projects {
        let (mut list, visited) = load_tasks_for(&p.id);
        visited_all.extend(visited);
        // Newest activity at the top, regardless of creation order.
        // Tie-break on task_id (which encodes a unix-nanos timestamp and is
        // lexicographically sortable) so two tasks reported in the same second
        // hold a stable order across rescans — otherwise the cursor jiggles.
        list.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then(b.task_id.cmp(&a.task_id))
        });
        // Walk the same task ids for pr.json before we move `list`.
        for t in &list {
            if let Some((path, summary)) = load_pr_summary(&p.id, &t.task_id) {
                visited_prs.insert(path);
                pr_summaries.insert(t.task_id.clone(), summary);
            }
        }
        tasks.insert(p.id.clone(), list);
        // IO errors are render-side-noise: treat as no holder so a transient
        // glitch doesn't paint every Merging card with the queued style.
        let holder = merge_lock::current_holder(&p.id).unwrap_or(None);
        let holder_pr_id = holder
            .as_ref()
            .and_then(|h| crate::pr::read_pr(&p.id, &h.task_id).ok().flatten())
            .map(|pr| pr.id);
        merge_lock_holders.insert(p.id.clone(), holder);
        merge_lock_holder_pr_ids.insert(p.id.clone(), holder_pr_id);
    }

    // Evict cache entries for paths not seen this scan (deleted tasks,
    // removed projects). Scoped so the lock isn't held across IO.
    {
        let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
        c.retain(|k, _| visited_all.contains(k));
    }
    {
        let mut c = pr_cache().lock().unwrap_or_else(|e| e.into_inner());
        c.retain(|k, _| visited_prs.contains(k));
    }

    ProjectsSnapshot {
        projects,
        tasks,
        titling: HashSet::new(),
        merge_lock_holders,
        merge_lock_holder_pr_ids,
        pr_summaries,
    }
}

/// Load (or cache-hit) the PR summary for one task. Returns the canonical
/// `pr.json` path alongside the summary so callers can track visitation
/// for cache eviction. Filesystem errors are logged and treated as
/// "no PR" — render noise must never escalate to a panic.
fn load_pr_summary(project_id: &str, task_id: &str) -> Option<(PathBuf, PrCardSummary)> {
    let path = crate::pr::pr_file_path(project_id, task_id)?;
    let meta = match fs::metadata(&path) {
        Ok(m) => m,
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound {
                log::warn!("projects_scan: pr stat error at {}: {}", path.display(), e);
            }
            return None;
        }
    };
    if !meta.is_file() {
        return None;
    }
    let mtime = match meta.modified() {
        Ok(m) => m,
        Err(e) => {
            log::warn!(
                "projects_scan: pr mtime unavailable at {}: {}",
                path.display(),
                e
            );
            return None;
        }
    };
    {
        let c = pr_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((cached_mtime, summary)) = c.get(&path) {
            if *cached_mtime == mtime {
                return Some((path, *summary));
            }
        }
    }
    match crate::pr::read_pr(project_id, task_id) {
        Ok(Some(pr)) => {
            let summary = PrCardSummary {
                id: pr.id,
                review_state: pr.review_state,
                comments: pr.comments.len().min(u16::MAX as usize) as u16,
            };
            {
                let mut c = pr_cache().lock().unwrap_or_else(|e| e.into_inner());
                c.insert(path.clone(), (mtime, summary));
            }
            Some((path, summary))
        }
        Ok(None) => None,
        Err(e) => {
            log::warn!("projects_scan: pr parse error at {}: {}", path.display(), e);
            None
        }
    }
}

fn load_tasks_for(project_id: &str) -> (Vec<Arc<TaskState>>, HashSet<PathBuf>) {
    let Some(dir) = orchestrator::project_state_dir(project_id) else {
        return (Vec::new(), HashSet::new());
    };
    let tasks_dir = dir.join("tasks");
    load_tasks_from_dir(&tasks_dir)
}

/// Walk-and-parse keyed off an explicit `tasks_dir` so tests can drive it
/// against a tempdir. The visited set is the cache-invalidation truth:
/// any cache entry not in it across the full scan gets evicted.
fn load_tasks_from_dir(tasks_dir: &Path) -> (Vec<Arc<TaskState>>, HashSet<PathBuf>) {
    let entries = match fs::read_dir(tasks_dir) {
        Ok(it) => it,
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound {
                log::warn!(
                    "projects_scan: read_dir error at {}: {}",
                    tasks_dir.display(),
                    e
                );
            }
            return (Vec::new(), HashSet::new());
        }
    };
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                log::warn!(
                    "projects_scan: dir entry error at {}: {}",
                    tasks_dir.display(),
                    e
                );
                continue;
            }
        };
        let path = entry.path().join("state.json");
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                if e.kind() != io::ErrorKind::NotFound {
                    log::warn!("projects_scan: stat error at {}: {}", path.display(), e);
                }
                continue;
            }
        };
        if !meta.is_file() {
            continue;
        }
        let mtime = match meta.modified() {
            Ok(m) => m,
            Err(e) => {
                log::warn!(
                    "projects_scan: mtime unavailable at {}: {}",
                    path.display(),
                    e
                );
                continue;
            }
        };
        visited.insert(path.clone());

        // Scoped so the lock is dropped before the read+parse below, never
        // held across IO.
        {
            let c = cache().lock().unwrap_or_else(|e| e.into_inner());
            if let Some((cached_mtime, state)) = c.get(&path) {
                if *cached_mtime == mtime {
                    out.push(Arc::clone(state));
                    continue;
                }
            }
        }

        match fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<TaskState>(&raw) {
                Ok(state) => {
                    let arc = Arc::new(state);
                    {
                        let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
                        c.insert(path.clone(), (mtime, Arc::clone(&arc)));
                    }
                    out.push(arc);
                }
                Err(e) => log::warn!("projects_scan: parse error at {}: {}", path.display(), e),
            },
            Err(e) => log::warn!("projects_scan: read error at {}: {}", path.display(), e),
        }
    }
    (out, visited)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_state(path: &Path, prompt: &str, updated_at: i64) {
        let json = format!(
            r#"{{
                "task_id": "t-1",
                "project_id": "p-1",
                "project_root": "/tmp/p-1",
                "status": "running",
                "prompt": "{}",
                "created_at": 1,
                "updated_at": {}
            }}"#,
            prompt, updated_at
        );
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, json).unwrap();
    }

    #[test]
    fn scan_populates_pr_summaries_from_pr_json() {
        use crate::test_util::HOME_TEST_LOCK;
        let _guard = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        // Set up a registered project + one task with a PR. `scan()` walks
        // projects.toml and the task tree under `$HOME/.cc-hub`.
        let project_id = "p-scan-pr";
        let project_root = home.path().join("proj");
        fs::create_dir_all(&project_root).expect("project root");
        orchestrator::save_projects(&orchestrator::ProjectsFile {
            projects: vec![orchestrator::Project {
                id: project_id.into(),
                name: "scan-pr".into(),
                root: project_root.clone(),
                created_at: 0,
                build_cmd: None,
            }],
        })
        .expect("save_projects");

        let mut state =
            orchestrator::TaskState::new(project_id.into(), project_root.clone(), "do".into());
        state.task_id = "t-scan-1".into();
        orchestrator::write_task_state(&state).expect("write_task_state");

        // Drive write_pr through the public helper so the on-disk schema
        // matches what real PRs produce.
        let mut pr = crate::pr::create_pr(
            &state,
            "cc-hub/t-scan-1".into(),
            "main".into(),
            "T".into(),
            "D".into(),
        )
        .expect("create_pr");
        pr.review_state = crate::pr::ReviewState::ChangesRequested;
        pr.comments.push(crate::pr::Comment {
            author: "user".into(),
            at: 0,
            body: "please fix".into(),
        });
        pr.comments.push(crate::pr::Comment {
            author: "user".into(),
            at: 0,
            body: "another note".into(),
        });
        crate::pr::write_pr(&pr).expect("write_pr");

        let snap = scan();
        let summary = snap
            .pr_summaries
            .get("t-scan-1")
            .copied()
            .expect("pr_summaries entry present");
        assert_eq!(summary.id, pr.id);
        assert_eq!(
            summary.review_state,
            crate::pr::ReviewState::ChangesRequested
        );
        assert_eq!(summary.comments, 2);

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn cache_returns_fresh_value_after_mtime_change() {
        let dir = tempdir().unwrap();
        let tasks_dir = dir.path().to_path_buf();
        let state_path = tasks_dir.join("t-1").join("state.json");

        write_state(&state_path, "first", 100);
        let (list, visited) = load_tasks_from_dir(&tasks_dir);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].prompt, "first");
        assert!(visited.contains(&state_path));

        // Second call without modification — cache hit, same value.
        let (list, _) = load_tasks_from_dir(&tasks_dir);
        assert_eq!(list[0].prompt, "first");

        // Bump mtime past filesystem resolution (ext4 is ns, but be safe
        // for slower mtime-granularity filesystems if anyone runs the
        // test elsewhere) and rewrite. Cache entry must be replaced.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_state(&state_path, "second", 200);
        let (list, _) = load_tasks_from_dir(&tasks_dir);
        assert_eq!(list[0].prompt, "second");
        assert_eq!(list[0].updated_at, 200);
    }
}
