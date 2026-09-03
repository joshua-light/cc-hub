//! One tick: spawn `claude -p`, wait for it, parse what happened.
//!
//! Everything here is confined to argv construction and stream-json parsing,
//! so swapping the runtime later means rewriting this module and nothing
//! else. The CLI is driven as a subprocess (not the SDK) because it reuses
//! the machine's existing credentials.

use super::spec::{Spec, AUTOCOMPACT_FLOOR};
use super::tools;
use log::warn;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default)]
pub struct Tick {
    pub ok: bool,
    /// The `result` message's subtype: `success`, `error_max_turns`,
    /// `error_max_budget_usd`, … or `timeout` / `spawn_failed` from here.
    pub subtype: Option<String>,
    pub session_id: Option<String>,
    pub compactions: u32,
    pub turns: u32,
    pub cost_usd: f64,
    /// Final assistant text on success, joined `errors` on failure.
    pub result: String,
    /// Context at the tick's first model call (the fixed prefix).
    pub context_start: u64,
    /// Context at its last call, after the conversation accumulated.
    pub context_end: u64,
    pub returncode: i32,
    pub stderr: String,
    pub duration_s: u64,
}

impl Tick {
    /// The CLI aborts when context refills within 3 turns of a compact,
    /// 3× running. Such a session can never recover.
    pub fn thrashed(&self) -> bool {
        self.result.contains("Autocompact is thrashing")
    }

    /// Budget went to compaction rather than work — the window is too small.
    pub fn compaction_starved(&self) -> bool {
        self.subtype.as_deref() == Some("error_max_budget_usd") && self.compactions >= 2
    }
}

/// Arguments after the spawn command. Pure, so the exact CLI contract is
/// pinned by tests.
pub fn build_args(
    spec: &Spec,
    prompt: &str,
    resume: Option<&str>,
    prompt_file: &Path,
) -> Vec<String> {
    let (allow, deny) = tools::scope(&spec.run.tools).unwrap_or_default();
    let mut argv: Vec<String> = vec![
        "-p".into(),
        prompt.into(),
        "--system-prompt-file".into(),
        prompt_file.display().to_string(),
        "--autocompact".into(),
        AUTOCOMPACT_FLOOR.to_string(),
        // Unlisted tools are denied, never prompted into the void.
        "--permission-mode".into(),
        "dontAsk".into(),
        // Don't inherit ambient MCP servers.
        "--strict-mcp-config".into(),
        "--setting-sources".into(),
        spec.run.setting_sources.clone(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--max-budget-usd".into(),
        spec.run.max_budget_usd.to_string(),
    ];
    if !allow.is_empty() {
        argv.push("--allowed-tools".into());
        argv.extend(allow);
    }
    if !deny.is_empty() {
        argv.push("--disallowed-tools".into());
        argv.extend(deny);
    }
    if let Some(r) = resume {
        argv.push("--resume".into());
        argv.push(r.into());
    }
    if let Some(n) = spec.run.max_turns {
        argv.push("--max-turns".into());
        argv.push(n.to_string());
    }
    if let Some(m) = &spec.run.model {
        argv.push("--model".into());
        argv.push(m.clone());
    }
    if let Some(e) = &spec.run.effort {
        argv.push("--effort".into());
        argv.push(e.clone());
    }
    argv
}

/// Run one tick to completion. `log_path` receives the raw stream-json,
/// prefixed by a `cc-hub-tick` header line so ticks in one day file can be
/// told apart.
pub fn run(
    spec: &Spec,
    prompt: &str,
    resume: Option<&str>,
    log_path: Option<&Path>,
    header: &str,
) -> Tick {
    let started = Instant::now();

    // A long system prompt as argv risks E2BIG, so it always goes through a
    // file. It lives in the agent dir, never the workdir.
    let prompt_file: PathBuf = spec.dir.join(".system-prompt.txt");
    if let Err(e) = fs::write(&prompt_file, &spec.system_prompt) {
        return Tick {
            subtype: Some("spawn_failed".into()),
            result: format!("write system prompt: {}", e),
            returncode: -1,
            ..Default::default()
        };
    }
    if let Err(e) = fs::create_dir_all(&spec.workdir) {
        return Tick {
            subtype: Some("spawn_failed".into()),
            result: format!("create workdir {}: {}", spec.workdir.display(), e),
            returncode: -1,
            ..Default::default()
        };
    }

    let spawn = crate::title::spawn_argv().unwrap_or_else(|| vec!["claude".into()]);
    // Agents report back through `cc-hub agent note`; make sure the `cc-hub`
    // they find is the one running them, even when it isn't on PATH.
    let path = child_path();
    let (exe, base) = spawn.split_first().expect("non-empty argv");
    let mut cmd = Command::new(exe);
    cmd.args(base)
        .args(build_args(spec, prompt, resume, &prompt_file))
        .current_dir(&spec.workdir)
        .env(
            "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE",
            spec.run.window_pct.to_string(),
        )
        .env("CC_HUB_AGENT", &spec.name)
        .env("CC_HUB_AGENT_DIR", &spec.dir)
        .env("PATH", path)
        .envs(&spec.run.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = crate::title::run_with_timeout(cmd, Duration::from_secs(spec.run.timeout_s));
    let duration_s = started.elapsed().as_secs();
    let Some(output) = output else {
        return Tick {
            subtype: Some("timeout".into()),
            result: format!("tick exceeded {}s or failed to spawn", spec.run.timeout_s),
            returncode: -1,
            duration_s,
            ..Default::default()
        };
    };

    let raw = String::from_utf8_lossy(&output.stdout);
    if let Some(p) = log_path {
        if let Err(e) = append_log(p, header, &raw) {
            warn!("harness[{}]: log write failed: {}", spec.name, e);
        }
    }
    let mut tick = parse(
        &raw,
        output.status.code().unwrap_or(-1),
        &String::from_utf8_lossy(&output.stderr),
    );
    tick.duration_s = duration_s;
    tick
}

/// PATH for the child: our own binary's directory first, then the
/// inherited PATH.
fn child_path() -> std::ffi::OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let Some(own) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    else {
        return inherited;
    };
    let mut paths = vec![own];
    paths.extend(std::env::split_paths(&inherited));
    std::env::join_paths(paths).unwrap_or(inherited)
}

fn append_log(path: &Path, header: &str, raw: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{}", header)?;
    f.write_all(raw.as_bytes())?;
    if !raw.ends_with('\n') {
        writeln!(f)?;
    }
    Ok(())
}

/// Fold a stream-json transcript into a [`Tick`].
pub fn parse(raw: &str, returncode: i32, stderr: &str) -> Tick {
    let mut tick = Tick {
        returncode,
        stderr: tail(stderr, 2000),
        ..Default::default()
    };
    for line in raw.lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let kind = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let subtype = msg.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
        match (kind, subtype) {
            ("system", "init") => {
                if let Some(id) = msg.get("session_id").and_then(|v| v.as_str()) {
                    tick.session_id = Some(id.to_string());
                }
            }
            (_, "compact_boundary") => tick.compactions += 1,
            ("assistant", _) => {
                tick.turns += 1;
                // The result message's usage sums every request in the tick,
                // so per-call context comes from the assistant messages.
                let used = msg
                    .get("message")
                    .and_then(|m| m.get("usage"))
                    .map(context_of)
                    .unwrap_or(0);
                if used > 0 {
                    if tick.context_start == 0 {
                        tick.context_start = used;
                    }
                    tick.context_end = used;
                }
            }
            ("result", _) => {
                tick.subtype = Some(subtype.to_string());
                tick.ok = subtype == "success";
                if let Some(id) = msg.get("session_id").and_then(|v| v.as_str()) {
                    tick.session_id = Some(id.to_string());
                }
                tick.cost_usd = msg
                    .get("total_cost_usd")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                // `result` is only populated on success; failures carry
                // `errors` instead.
                let result = msg.get("result").and_then(|v| v.as_str()).unwrap_or("");
                tick.result = if result.is_empty() {
                    msg.get("errors")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|e| e.as_str())
                                .collect::<Vec<_>>()
                                .join("; ")
                        })
                        .unwrap_or_default()
                } else {
                    result.to_string()
                };
            }
            _ => {}
        }
    }
    if tick.subtype.is_none() {
        tick.subtype = Some(
            if returncode == 0 {
                "no_result"
            } else {
                "crashed"
            }
            .into(),
        );
        if tick.result.is_empty() {
            tick.result = if tick.stderr.is_empty() {
                format!("exit {} without a result message", returncode)
            } else {
                tick.stderr.lines().last().unwrap_or("").to_string()
            };
        }
    }
    tick
}

/// Total input context for one model call: fresh + cache-written + cache-read.
fn context_of(usage: &serde_json::Value) -> u64 {
    [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .iter()
    .map(|k| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0))
    .sum()
}

fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_string();
    }
    chars[chars.len() - n..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::spec;

    fn spec_with(raw: &str) -> Spec {
        spec::parse(Path::new("/tmp/agents/t"), raw).unwrap()
    }

    #[test]
    fn args_pin_the_cli_contract() {
        let s = spec_with(
            "[run]\ntools=[\"Read\",\"Bash(git *)\"]\nmax_turns=7\nmodel=\"sonnet\"\n[prompt]\ninstruction=\"go\"",
        );
        let args = build_args(&s, "hello", Some("sid"), Path::new("/p/sys.txt"));
        let joined = args.join(" ");
        assert!(joined.starts_with("-p hello --system-prompt-file /p/sys.txt --autocompact 100000 --permission-mode dontAsk --strict-mcp-config --setting-sources  --output-format stream-json --verbose --max-budget-usd 1"));
        assert!(joined.contains("--allowed-tools Read Bash(git *)"));
        assert!(joined.contains("--disallowed-tools"));
        assert!(joined.contains(" Write "));
        assert!(!joined.contains(" Read Write"), "Read must not be denied");
        assert!(joined.ends_with("--resume sid --max-turns 7 --model sonnet"));
    }

    #[test]
    fn parse_success() {
        let raw = r#"{"type":"system","subtype":"init","session_id":"abc"}
{"type":"assistant","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":2000}}}
{"type":"system","subtype":"compact_boundary"}
{"type":"assistant","message":{"usage":{"input_tokens":50,"cache_creation_input_tokens":100}}}
{"type":"result","subtype":"success","session_id":"abc","total_cost_usd":0.12,"result":"DONE"}"#;
        let t = parse(raw, 0, "");
        assert!(t.ok);
        assert_eq!(t.session_id.as_deref(), Some("abc"));
        assert_eq!(t.turns, 2);
        assert_eq!(t.compactions, 1);
        assert_eq!(t.context_start, 2010);
        assert_eq!(t.context_end, 150);
        assert_eq!(t.result, "DONE");
        assert!((t.cost_usd - 0.12).abs() < 1e-9);
    }

    #[test]
    fn parse_failure_uses_errors() {
        let raw = r#"{"type":"result","subtype":"error_max_turns","total_cost_usd":0.5,"errors":["hit max turns"]}"#;
        let t = parse(raw, 0, "");
        assert!(!t.ok);
        assert_eq!(t.subtype.as_deref(), Some("error_max_turns"));
        assert_eq!(t.result, "hit max turns");
    }

    #[test]
    fn parse_without_result_reports_crash() {
        let t = parse("not json\n", 1, "boom: bad flag\n");
        assert!(!t.ok);
        assert_eq!(t.subtype.as_deref(), Some("crashed"));
        assert_eq!(t.result, "boom: bad flag");
    }

    #[test]
    fn thrash_detection() {
        let t = Tick {
            result: "Autocompact is thrashing".into(),
            ..Default::default()
        };
        assert!(t.thrashed());
        let s = Tick {
            subtype: Some("error_max_budget_usd".into()),
            compactions: 2,
            ..Default::default()
        };
        assert!(s.compaction_starved());
    }
}
