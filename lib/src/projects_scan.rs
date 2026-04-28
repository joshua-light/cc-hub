//! Scanner for the Projects tab.
//!
//! Reads `~/.cc-hub/projects.toml` for the registered project list, then
//! walks `~/.cc-hub/projects/<project-id>/tasks/<task-id>/state.json` for
//! every task. Returns a flat snapshot the TUI can render directly without
//! further IO.
//!
//! Cheap enough to call on each fs-watcher tick: typical project count is a
//! handful, tasks per project rarely more than a few dozen, each state file
//! is a few KB. We deliberately don't cache between scans — staleness is
//! the failure mode that matters here, not redundant IO.

use crate::orchestrator::{self, Project, TaskState, TaskStatus};
use crate::reservations::{self, Phase, Reservation};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;

/// Where a tmux session sits in the orchestrator hierarchy. Computed by
/// matching the session's tmux name against every TaskState in the latest
/// snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionRole {
    /// This session is itself an orchestrator for `task_id`.
    Orchestrator { task_id: String, project_id: String },
    /// This session is a worker spawned by the orchestrator for `task_id`.
    Worker {
        task_id: String,
        project_id: String,
        worktree: Option<String>,
        readonly: bool,
    },
}

#[derive(Debug, Clone)]
pub struct ProjectsSnapshot {
    pub projects: Vec<Project>,
    /// Tasks for each project_id, sorted newest-first by `updated_at` so
    /// the UI shows the most recently active task at the top.
    pub tasks: HashMap<String, Vec<TaskState>>,
    /// Task ids whose Haiku titler is in flight right now. Populated in
    /// the main loop before the snapshot is handed to the App so the UI
    /// can render a spinner without locking a shared mutex per frame.
    pub titling: HashSet<String>,
    /// Live (non-stale) reservations keyed by project_id. Read once per
    /// scan from `~/.cc-hub/projects/<id>/reservations.json` so card
    /// rendering doesn't touch the filesystem.
    pub reservations: HashMap<String, Vec<Reservation>>,
}

/// One pair of live reservations whose path sets overlap. Always ordered
/// holder-first (the `Active` side); the waiter is whichever other live
/// reservation collides on those paths. Surfaced in the projects body so
/// the user can see at-a-glance which tasks are stepping on each other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contention {
    pub holder_task: String,
    pub waiter_task: String,
    pub overlapping_paths: Vec<String>,
}

impl ProjectsSnapshot {
    pub fn empty() -> Self {
        Self {
            projects: Vec::new(),
            tasks: HashMap::new(),
            titling: HashSet::new(),
            reservations: HashMap::new(),
        }
    }

    /// Best live reservation for a task — `Active` wins over `Intended` if
    /// the task somehow has both (it shouldn't in normal flow, but a worker
    /// upgrade-then-downgrade can leave both present briefly).
    pub fn reservation_for_task(
        &self,
        project_id: &str,
        task_id: &str,
    ) -> Option<&Reservation> {
        let list = self.reservations.get(project_id)?;
        let mut best: Option<&Reservation> = None;
        for r in list {
            if r.task_id != task_id {
                continue;
            }
            match (best, &r.phase) {
                (None, _) => best = Some(r),
                (Some(b), Phase::Active) if b.phase != Phase::Active => best = Some(r),
                _ => {}
            }
        }
        best
    }

    /// Pairs of overlapping live reservations within a project. Each
    /// unordered pair appears exactly once, holder = the `Active` side,
    /// waiter = whichever other reservation collides. Sorted for stable
    /// rendering. O(N²) over reservations, which is fine — N is small.
    pub fn contentions_for(&self, project_id: &str) -> Vec<Contention> {
        let Some(list) = self.reservations.get(project_id) else {
            return Vec::new();
        };
        let mut out: Vec<Contention> = Vec::new();
        for (i, holder) in list.iter().enumerate() {
            if holder.phase != Phase::Active {
                continue;
            }
            for (j, other) in list.iter().enumerate() {
                if i == j || holder.task_id == other.task_id {
                    continue;
                }
                // For Active-vs-Active, only emit once: keep the lower
                // index as holder. For Active-vs-Intended either order is
                // fine because Intended is never a holder.
                if other.phase == Phase::Active && j < i {
                    continue;
                }
                let mut hits: Vec<String> = Vec::new();
                for hp in &holder.paths {
                    for op in &other.paths {
                        if reservations::paths_overlap(hp, op) && !hits.contains(hp) {
                            hits.push(hp.clone());
                        }
                    }
                }
                if !hits.is_empty() {
                    out.push(Contention {
                        holder_task: holder.task_id.clone(),
                        waiter_task: other.task_id.clone(),
                        overlapping_paths: hits,
                    });
                }
            }
        }
        out.sort_by(|a, b| {
            a.holder_task
                .cmp(&b.holder_task)
                .then(a.waiter_task.cmp(&b.waiter_task))
        });
        out
    }

    pub fn active_task_count(&self, project_id: &str) -> usize {
        self.tasks
            .get(project_id)
            .map(|v| {
                v.iter()
                    .filter(|t| t.status == TaskStatus::Running)
                    .count()
            })
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
                    out.entry(name.to_string()).or_insert(SessionRole::Orchestrator {
                        task_id: t.task_id.clone(),
                        project_id: t.project_id.clone(),
                    });
                }
                for w in &t.workers {
                    out.entry(w.tmux_name.clone()).or_insert(SessionRole::Worker {
                        task_id: t.task_id.clone(),
                        project_id: t.project_id.clone(),
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
    let mut tasks: HashMap<String, Vec<TaskState>> = HashMap::new();
    let mut reservations_by_project: HashMap<String, Vec<Reservation>> = HashMap::new();

    for p in &projects {
        let mut list = load_tasks_for(&p.id);
        // Newest activity at the top, regardless of creation order.
        // Tie-break on task_id (which encodes a unix-nanos timestamp and is
        // lexicographically sortable) so two tasks reported in the same second
        // hold a stable order across rescans — otherwise the cursor jiggles.
        list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(b.task_id.cmp(&a.task_id)));
        tasks.insert(p.id.clone(), list);
        // Reservations file legitimately may not exist yet for projects
        // that have never declared any (NotFound is silent); any other IO
        // error is logged so it doesn't disappear into Vec::new().
        let live = match reservations::list(&p.id, false) {
            Ok(v) => v,
            Err(e) => {
                if e.kind() != io::ErrorKind::NotFound {
                    log::warn!(
                        "projects_scan: reservations.list error for project {}: {}",
                        p.id,
                        e
                    );
                }
                Vec::new()
            }
        };
        reservations_by_project.insert(p.id.clone(), live);
    }

    ProjectsSnapshot {
        projects,
        tasks,
        titling: HashSet::new(),
        reservations: reservations_by_project,
    }
}

fn load_tasks_for(project_id: &str) -> Vec<TaskState> {
    let Some(dir) = orchestrator::project_state_dir(project_id) else {
        return Vec::new();
    };
    let tasks_dir = dir.join("tasks");
    let entries = match fs::read_dir(&tasks_dir) {
        Ok(it) => it,
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound {
                log::warn!(
                    "projects_scan: read_dir error at {}: {}",
                    tasks_dir.display(),
                    e
                );
            }
            return Vec::new();
        }
    };
    let mut out = Vec::new();
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
        if !path.is_file() {
            continue;
        }
        match fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<TaskState>(&raw) {
                Ok(state) => out.push(state),
                Err(e) => log::warn!(
                    "projects_scan: parse error at {}: {}",
                    path.display(),
                    e
                ),
            },
            Err(e) => log::warn!(
                "projects_scan: read error at {}: {}",
                path.display(),
                e
            ),
        }
    }
    out
}
