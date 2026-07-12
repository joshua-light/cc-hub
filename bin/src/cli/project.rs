//! `cc-hub project list` — enumerate registered projects.

use super::{parse_flags, print_json, CliError};

pub(crate) fn project_subcommand(args: &[String]) -> Result<(), CliError> {
    let (verb, rest) = args
        .split_first()
        .ok_or_else(|| CliError::Usage("project <verb>: missing verb (try `list`)".into()))?;
    match verb.as_str() {
        "list" => project_list(rest),
        other => Err(CliError::Usage(format!(
            "unknown project verb: {} (try `list`)",
            other
        ))),
    }
}

/// `cc-hub project list [--json]`
///
/// Enumerate registered projects from `~/.cc-hub/projects.toml`. Plain
/// output is one tab-separated row per project: `<id>\t<name>\t<root>`.
/// With `--json`, one `{ok:true, projects:[{id, name, root,
/// task_counts:{backlog,running,review,merging,done}}]}` envelope. Sorted by
/// name (case-insensitive) so the listing is stable across machines.
fn project_list(args: &[String]) -> Result<(), CliError> {
    use cc_hub_lib::orchestrator::TaskStatus;
    use cc_hub_lib::projects_scan;

    let f = parse_flags(args)?;
    let mut snap = projects_scan::scan();

    let mut projects = std::mem::take(&mut snap.projects);
    projects.sort_by_key(|a| a.name.to_lowercase());

    if f.json {
        let arr: Vec<serde_json::Value> = projects
            .iter()
            .map(|p| {
                let tasks = snap.tasks.get(&p.id).map(|v| v.as_slice()).unwrap_or(&[]);
                let mut backlog = 0usize;
                let mut running = 0usize;
                let mut review = 0usize;
                let mut merging = 0usize;
                let mut done = 0usize;
                for t in tasks {
                    match t.status {
                        TaskStatus::Backlog => backlog += 1,
                        // Personal-board state; never in a project scan.
                        TaskStatus::Planning => running += 1,
                        TaskStatus::Running => running += 1,
                        TaskStatus::Review => review += 1,
                        TaskStatus::Merging => merging += 1,
                        TaskStatus::Done => done += 1,
                    }
                }
                serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    "root": p.root,
                    "task_counts": {
                        "backlog": backlog,
                        "running": running,
                        "review": review,
                        "merging": merging,
                        "done": done,
                    },
                })
            })
            .collect();
        print_json(&serde_json::json!({ "ok": true, "projects": arr }));
    } else {
        // Tab-separated so consumers can split on \t even if a name contains spaces.
        for p in &projects {
            println!("{}\t{}\t{}", p.id, p.name, p.root.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::cli::dispatch;

    #[test]
    fn dispatch_parses_project_list_json() {
        let argv = vec![
            "project".to_string(),
            "list".to_string(),
            "--json".to_string(),
        ];
        let code = dispatch(&argv);
        assert_eq!(
            code,
            Some(0),
            "expected dispatch to handle 'project list --json' cleanly"
        );
    }
}
