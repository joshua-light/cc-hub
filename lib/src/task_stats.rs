//! Persisted task usage. Missing transcripts are unknown, never zero-cost work.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    /// USD at the Metrics tab's rates, or the provider's reported cost.
    /// None if any contributing call has no supported pricing.
    pub cost_nano_usd: Option<u64>,
    pub estimated: bool,
    pub sessions: usize,
    /// Source paths retain the last complete snapshot if logs are removed.
    pub sources: BTreeSet<String>,
}

impl TaskStats {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_creation_tokens
    }

    pub fn cost_label(&self) -> String {
        match self.cost_nano_usd {
            Some(cost) => format!(
                "{}${:.4}",
                if self.estimated { "~" } else { "" },
                cost as f64 / 1_000_000_000.0
            ),
            None => "cost unavailable".into(),
        }
    }

    pub fn compact(&self) -> String {
        let total = self.total_tokens();
        let tokens = if total >= 1_000_000 {
            format!("{:.1}M", total as f64 / 1_000_000.0)
        } else if total >= 10_000 {
            format!("{:.1}K", total as f64 / 1_000.0)
        } else {
            total.to_string()
        };
        format!("{tokens} tokens · {}", self.cost_label())
    }
}

/// Runs on a background thread; file parsing never blocks input or drawing.
pub fn refresh() -> Vec<(String, TaskStats)> {
    let Ok(board) = crate::tasks::PersonalBoard::load_result() else {
        return Vec::new();
    };
    let links = crate::session_tasks::load();
    let files = crate::metrics::task_usage_files();
    let mut result = Vec::new();
    for task in board.tasks() {
        let mut ids: BTreeSet<String> = task.usage_session_ids.iter().cloned().collect();
        ids.extend(task.session_id.iter().cloned());
        ids.extend(
            links
                .iter()
                .filter(|(_, link)| link.project_id.is_none() && link.task_id == task.task_id)
                .map(|(id, _)| id.clone()),
        );
        ids.retain(|id| !id.is_empty());
        if ids.is_empty() {
            continue;
        }
        let Some(stats) = crate::metrics::task_usage(&ids, &files) else {
            continue;
        };
        // Preserve the last complete result after transcript cleanup.
        if task
            .stats
            .as_ref()
            .is_some_and(|old| !old.sources.is_subset(&stats.sources))
        {
            continue;
        }
        if task.stats.as_ref() == Some(&stats) {
            result.push((task.task_id.clone(), stats));
        } else {
            match crate::orchestrator::set_task_stats(&task.task_id, stats.clone()) {
                Ok(_) => result.push((task.task_id.clone(), stats)),
                Err(error) => log::warn!("task stats {}: {error}", task.task_id),
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{agent::AgentKind, metrics};
    use serde_json::json;
    use std::path::Path;

    fn claude(path: &Path, request: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let line = json!({"type":"assistant", "requestId":request,
            "message":{"id":request,"model":"claude-sonnet-4-6",
                "usage":{"input_tokens":100,"output_tokens":10,
                    "cache_read_input_tokens":50,"cache_creation_input_tokens":20}}});
        // Claude emits the same usage for multiple content blocks.
        std::fs::write(path, format!("{line}\n{line}\n")).unwrap();
    }

    #[test]
    fn includes_reassignments_and_subagents_without_duplicate_calls() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.jsonl");
        let second = dir.path().join("second.jsonl");
        let sub = dir.path().join("first/subagents/agent-one.jsonl");
        claude(&first, "request-a");
        claude(&second, "request-a"); // copied history on resume
        claude(&sub, "request-b");
        let files = vec![
            (first, false, AgentKind::Claude),
            (second, false, AgentKind::Claude),
            (sub, true, AgentKind::Claude),
        ];
        let stats = metrics::task_usage(&BTreeSet::from(["first".into(), "second".into()]), &files)
            .unwrap();
        assert_eq!(stats.total_tokens(), 360);
        assert_eq!(stats.cost_nano_usd, Some(1_080_000));
        assert!(stats.estimated);
        assert_eq!(stats.sessions, 3);
    }

    #[test]
    fn missing_session_is_not_a_partial_or_zero_total() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("first.jsonl");
        claude(&path, "request-a");
        assert!(metrics::task_usage(
            &BTreeSet::from(["first".into(), "missing".into()]),
            &[(path, false, AgentKind::Claude)]
        )
        .is_none());
    }

    #[test]
    fn pi_uses_reported_cost_and_codex_uses_final_cumulative_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let pi = dir.path().join("date_pi-id.jsonl");
        let message = json!({"type":"message", "message":{"role":"assistant", "model":"custom-model",
            "usage":{"input":100,"output":10,"cacheRead":20,"cacheWrite":5,"cost":{"total":0.25}}}});
        std::fs::write(&pi, format!("{message}\n")).unwrap();
        let stats = metrics::task_usage(
            &BTreeSet::from(["pi-id".into()]),
            &[(pi, false, AgentKind::Pi)],
        )
        .unwrap();
        assert_eq!(stats.total_tokens(), 135);
        assert_eq!(stats.cost_nano_usd, Some(250_000_000));
        assert!(!stats.estimated);

        let codex = dir.path().join("rollout-date-codex-id.jsonl");
        let usage = json!({"type":"event_msg", "payload":{"type":"token_count", "info":{"total_token_usage":{
            "input_tokens":100,"cached_input_tokens":40,"output_tokens":30,"reasoning_output_tokens":20}}}});
        std::fs::write(&codex, format!("{usage}\n{usage}\n")).unwrap();
        let stats = metrics::task_usage(
            &BTreeSet::from(["codex-id".into()]),
            &[(codex, false, AgentKind::Codex)],
        )
        .unwrap();
        assert_eq!(stats.total_tokens(), 130);
        assert_eq!(stats.cache_read_tokens, 40);
        assert_eq!(stats.cost_nano_usd, None);
    }

    #[cfg(unix)]
    #[test]
    fn refresh_persists_history_without_touching_task_and_survives_cleanup() {
        crate::test_util::with_temp_home(|| {
            let mut board = crate::tasks::PersonalBoard::load();
            let id = board.add("stats task").unwrap().unwrap();
            board
                .bind_resource(&id, "/tmp", "claude", "mux", Some("first"))
                .unwrap();
            board
                .bind_resource(&id, "/tmp", "claude", "mux2", Some("second"))
                .unwrap();
            let before = crate::orchestrator::read_task_state_for(None, &id).unwrap();
            assert_eq!(before.usage_session_ids, ["first", "second"]);
            let project = crate::platform::paths::claude_home()
                .unwrap()
                .join("projects/test");
            claude(&project.join("first.jsonl"), "request-a");
            claude(&project.join("second.jsonl"), "request-b");
            let refreshed = refresh();
            assert_eq!(refreshed.len(), 1);
            let saved = crate::orchestrator::read_task_state_for(None, &id).unwrap();
            assert_eq!(saved.updated_at, before.updated_at);
            assert_eq!(saved.stats.as_ref().unwrap().total_tokens(), 360);
            std::fs::remove_file(project.join("first.jsonl")).unwrap();
            refresh();
            assert_eq!(
                crate::orchestrator::read_task_state_for(None, &id)
                    .unwrap()
                    .stats,
                saved.stats
            );
        });
    }
}
