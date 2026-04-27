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
use std::collections::{HashMap, HashSet};
use std::fs;

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
}

impl ProjectsSnapshot {
    pub fn empty() -> Self {
        Self {
            projects: Vec::new(),
            tasks: HashMap::new(),
            titling: HashSet::new(),
        }
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

    for p in &projects {
        let mut list = load_tasks_for(&p.id);
        // Newest activity at the top, regardless of creation order.
        list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        tasks.insert(p.id.clone(), list);
    }

    ProjectsSnapshot {
        projects,
        tasks,
        titling: HashSet::new(),
    }
}

fn load_tasks_for(project_id: &str) -> Vec<TaskState> {
    let Some(dir) = orchestrator::project_state_dir(project_id) else {
        return Vec::new();
    };
    let tasks_dir = dir.join("tasks");
    let entries = match fs::read_dir(&tasks_dir) {
        Ok(it) => it,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
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
