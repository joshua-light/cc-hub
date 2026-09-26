use crate::agent::AgentKind;
use crate::platform::paths;
use std::path::PathBuf;

pub(super) fn discover_session_files() -> Vec<(PathBuf, bool, AgentKind)> {
    let mut out = Vec::new();

    if let Some(projects_dir) = paths::claude_home().map(|d| d.join("projects")) {
        if let Ok(entries) = std::fs::read_dir(&projects_dir) {
            for project in entries.flatten() {
                let pdir = project.path();
                if !pdir.is_dir() {
                    continue;
                }
                let inner = match std::fs::read_dir(&pdir) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                for child in inner.flatten() {
                    let p = child.path();
                    if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        out.push((p, false, AgentKind::Claude));
                    } else if p.is_dir() {
                        let sub = p.join("subagents");
                        if sub.is_dir() {
                            if let Ok(sa) = std::fs::read_dir(&sub) {
                                for f in sa.flatten() {
                                    let fp = f.path();
                                    if fp.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                                        out.push((fp, true, AgentKind::Claude));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(pi_sessions) = paths::pi_sessions_dir() {
        if let Ok(projects) = std::fs::read_dir(&pi_sessions) {
            for project in projects.flatten() {
                let pdir = project.path();
                if !pdir.is_dir() {
                    continue;
                }
                let Ok(inner) = std::fs::read_dir(&pdir) else {
                    continue;
                };
                for child in inner.flatten() {
                    let p = child.path();
                    if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        out.push((p, false, AgentKind::Pi));
                    }
                }
            }
        }
    }

    // Codex rollouts are nested by date (`sessions/YYYY/MM/DD/rollout-*.jsonl`),
    // so walk the tree rather than a single level.
    if let Some(codex_sessions) = paths::codex_sessions_dir() {
        let mut stack = vec![codex_sessions];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in rd.flatten() {
                let p = entry.path();
                match entry.file_type() {
                    Ok(ft) if ft.is_dir() => stack.push(p),
                    Ok(ft) if ft.is_file() => {
                        let is_rollout = p.extension().and_then(|e| e.to_str()) == Some("jsonl")
                            && p.file_name()
                                .and_then(|n| n.to_str())
                                .is_some_and(|n| n.starts_with("rollout-"));
                        if is_rollout {
                            out.push((p, false, AgentKind::Codex));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    out
}
