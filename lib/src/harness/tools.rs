//! Tool scoping. The context prefix is dominated by tool schemas, so scoping
//! them is the main cost lever. Claude Code has no "allow only these"
//! switch: a bare-name deny rule removes a tool's schema from the request,
//! while an allow rule merely auto-approves one. So the problem is inverted
//! — deny everything in the registry the spec did not ask for.
//!
//! Measured in the reference harness: 26 tools ≈ 16.4k prefix tokens, 4
//! tools ≈ 6.3k.

/// Every built-in tool name the CLI may expose. Names absent from a given
/// build are harmless in a deny list, so this errs toward listing too many.
pub const REGISTRY: &[&str] = &[
    "Agent",
    "AskUserQuestion",
    "Bash",
    "BashOutput",
    "CronCreate",
    "CronDelete",
    "CronList",
    "DesignSync",
    "Edit",
    "EnterPlanMode",
    "EnterWorktree",
    "ExitPlanMode",
    "ExitWorktree",
    "Glob",
    "Grep",
    "KillShell",
    "ListAgents",
    "ListMcpResourcesTool",
    "Monitor",
    "NotebookEdit",
    "PushNotification",
    "Read",
    "ReadMcpResourceDirTool",
    "ReadMcpResourceTool",
    "RemoteTrigger",
    "ReportFindings",
    "ScheduleWakeup",
    "SendMessage",
    "SendUserFile",
    "Skill",
    "TaskCreate",
    "TaskOutput",
    "TaskStop",
    "TaskUpdate",
    "ToolSearch",
    "WebFetch",
    "WebSearch",
    "Workflow",
    "Write",
];

/// Denies every MCP tool across all servers unless a spec allows an
/// `mcp__*` entry explicitly.
const MCP_GLOB: &str = "mcp__*";

/// Everything the CLI offers: the whole registry allowed, nothing denied.
/// For an agent standing in for an interactive session, paying the full
/// prefix on purpose. It is the entire list or nothing — `["*", "Read"]`
/// would only mean the first one. MCP tools take no wildcard allow rule, so
/// reaching those needs `permission_mode = "bypassPermissions"` too.
pub const EVERYTHING: &str = "*";

/// `(allow_rules, deny_rules)` for a spec's tool list. Scoped rules like
/// `Bash(git *)` keep their base tool out of the deny list.
pub fn scope(allowed: &[String]) -> Result<(Vec<String>, Vec<String>), String> {
    if allowed.iter().any(|t| t == EVERYTHING) {
        if allowed.len() > 1 {
            return Err(format!("tools = [{:?}] takes no other entries", EVERYTHING));
        }
        return Ok((REGISTRY.iter().map(|t| t.to_string()).collect(), Vec::new()));
    }
    let mut bases: Vec<&str> = Vec::new();
    let mut mcp_allowed = false;
    for entry in allowed {
        let base = entry.split('(').next().unwrap_or("").trim();
        if base.starts_with("mcp__") {
            mcp_allowed = true;
            continue;
        }
        if !REGISTRY.contains(&base) {
            return Err(format!(
                "{:?} is not a known tool (add it to harness::tools::REGISTRY if the CLI gained it)",
                base
            ));
        }
        bases.push(base);
    }
    let mut deny: Vec<String> = REGISTRY
        .iter()
        .filter(|t| !bases.contains(t))
        .map(|t| t.to_string())
        .collect();
    if !mcp_allowed {
        deny.push(MCP_GLOB.into());
    }
    Ok((allowed.to_vec(), deny))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn denies_everything_not_allowed() {
        let (allow, deny) = scope(&s(&["Read", "Bash(git *)"])).unwrap();
        assert_eq!(allow, s(&["Read", "Bash(git *)"]));
        assert!(!deny.contains(&"Read".to_string()));
        assert!(!deny.contains(&"Bash".to_string()));
        assert!(deny.contains(&"Write".to_string()));
        assert!(deny.contains(&"mcp__*".to_string()));
    }

    #[test]
    fn mcp_entry_lifts_mcp_glob() {
        let (_, deny) = scope(&s(&["mcp__github__*"])).unwrap();
        assert!(!deny.contains(&"mcp__*".to_string()));
        assert_eq!(deny.len(), REGISTRY.len());
    }

    #[test]
    fn star_allows_everything_and_denies_nothing() {
        let (allow, deny) = scope(&s(&["*"])).unwrap();
        assert!(deny.is_empty());
        assert_eq!(allow.len(), REGISTRY.len());
        assert!(allow.contains(&"Bash".to_string()));
        assert!(scope(&s(&["*", "Read"])).is_err());
    }

    #[test]
    fn unknown_tool_is_an_error() {
        assert!(scope(&s(&["Nope"])).is_err());
    }
}
