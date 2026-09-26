use super::cost::{cost_of, known_pricing};
use super::discover::discover_session_files;
use super::parse::parse_session_file;
use crate::agent::AgentKind;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub(crate) fn task_usage_files() -> Vec<(PathBuf, bool, AgentKind)> {
    let mut files = discover_session_files();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

pub(crate) fn task_usage(
    ids: &std::collections::BTreeSet<String>,
    files: &[(PathBuf, bool, AgentKind)],
) -> Option<crate::tasks::stats::TaskStats> {
    let mut stats = crate::tasks::stats::TaskStats {
        cost_nano_usd: Some(0),
        ..Default::default()
    };
    let mut seen = HashSet::new();
    let mut found = HashSet::new();
    for (path, subagent, kind) in files {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let parent_id = path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|s| s.to_str());
        let matched = ids.iter().find(|id| {
            if *subagent {
                parent_id == Some(id.as_str())
            } else {
                stem == id.as_str()
                    || stem
                        .strip_suffix(id.as_str())
                        .is_some_and(|prefix| prefix.ends_with('-') || prefix.ends_with('_'))
            }
        });
        let Some(id) = matched else { continue };
        let Some(session) = parse_session_file(path, *subagent, *kind) else {
            continue;
        };
        if !*subagent {
            found.insert(id.clone());
        }
        stats.sources.insert(path.to_string_lossy().into_owned());
        stats.sessions += 1;
        for call in session.calls {
            if !call.dedup_key.is_empty() && !seen.insert(call.dedup_key) {
                continue;
            }
            stats.input_tokens += call.tokens.input;
            stats.output_tokens += call.tokens.output;
            stats.cache_read_tokens += call.tokens.cache_read;
            stats.cache_creation_tokens += call.tokens.cache_creation;
            let cost = call
                .cost_override
                .filter(|c| c.is_finite() && *c >= 0.0)
                .or_else(|| {
                    // Never apply Claude fallback prices to Codex or unknown models.
                    known_pricing(&call.model).map(|p| {
                        stats.estimated = true;
                        cost_of(&call.tokens, &p)
                    })
                });
            stats.cost_nano_usd = stats
                .cost_nano_usd
                .zip(cost)
                .map(|(total, cost)| total.saturating_add((cost * 1_000_000_000.0).round() as u64));
        }
    }
    // A partial total would understate the task's spend.
    if found.len() != ids.len() || stats.sources.is_empty() {
        None
    } else {
        Some(stats)
    }
}
