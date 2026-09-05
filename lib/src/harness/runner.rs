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
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
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
        // Never a prompt: an unattended tick has nobody to ask.
        "--permission-mode".into(),
        spec.run.permission_mode.clone(),
        "--setting-sources".into(),
        spec.run.setting_sources.clone(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--max-budget-usd".into(),
        spec.run.max_budget_usd.to_string(),
    ];
    if !spec.run.mcp {
        // Don't inherit ambient MCP servers.
        argv.push("--strict-mcp-config".into());
    }
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
/// told apart. `session_started` is called once, as soon as the CLI
/// announces the session id — long before the tick ends, so the Agents tab
/// can tail a tick in flight.
pub fn run(
    spec: &Spec,
    prompt: &str,
    resume: Option<&str>,
    log_path: Option<&Path>,
    header: &str,
    session_started: &dyn Fn(&str),
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

    let streamed = stream(
        cmd,
        Duration::from_secs(spec.run.timeout_s),
        session_started,
    );
    let duration_s = started.elapsed().as_secs();
    let Some(streamed) = streamed else {
        return Tick {
            subtype: Some("timeout".into()),
            result: format!("tick exceeded {}s or failed to spawn", spec.run.timeout_s),
            returncode: -1,
            duration_s,
            ..Default::default()
        };
    };

    if let Some(p) = log_path {
        if let Err(e) = append_log(p, header, &streamed.raw) {
            warn!("harness[{}]: log write failed: {}", spec.name, e);
        }
    }
    let mut tick = parse(&streamed.raw, streamed.code, &streamed.stderr);
    tick.duration_s = duration_s;
    tick
}

/// What a finished tick left on its pipes.
struct Streamed {
    raw: String,
    stderr: String,
    code: i32,
}

/// Spawn the tick and drain its pipes while it runs.
///
/// Draining as it goes is not a nicety: a tick's stream-json outgrows the
/// 64KB pipe buffer within a few turns, and a child blocked writing into a
/// full pipe never exits — it dies at the timeout with nothing to show. It
/// also means the `init` line, the first thing the CLI prints, hands us the
/// session id while the tick is still running.
fn stream(mut cmd: Command, timeout: Duration, session_started: &dyn Fn(&str)) -> Option<Streamed> {
    crate::title::detach_from_tty(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| warn!("harness: spawn failed: {}", e))
        .ok()?;
    let (stdout, stderr) = (child.stdout.take()?, child.stderr.take()?);

    let (tx, rx) = mpsc::channel::<String>();
    let out = thread::spawn(move || {
        let mut raw = String::new();
        let mut announced = false;
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if !announced {
                if let Some(id) = session_id_of(&line) {
                    announced = tx.send(id).is_ok();
                }
            }
            raw.push_str(&line);
            raw.push('\n');
        }
        raw
    });
    let err = thread::spawn(move || {
        let mut s = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut s);
        s
    });

    let deadline = Instant::now() + timeout;
    let code = loop {
        for id in rx.try_iter() {
            session_started(&id);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(-1),
            Ok(None) => {
                if crate::title::shutting_down() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                if Instant::now() >= deadline {
                    warn!("harness: tick timed out after {:?}, killing", timeout);
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                warn!("harness: try_wait failed: {}", e);
                return None;
            }
        }
    };
    for id in rx.try_iter() {
        session_started(&id);
    }
    Some(Streamed {
        raw: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
        code,
    })
}

/// The session id off one stream-json line, if it carries one. The `init`
/// line does, so the first hit is the session the tick runs in.
fn session_id_of(line: &str) -> Option<String> {
    let msg: serde_json::Value = serde_json::from_str(line).ok()?;
    msg.get("session_id")?.as_str().map(str::to_string)
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
        assert!(joined.starts_with("-p hello --system-prompt-file /p/sys.txt --autocompact 100000 --permission-mode dontAsk --setting-sources  --output-format stream-json --verbose --max-budget-usd 1 --strict-mcp-config"));
        assert!(joined.contains("--allowed-tools Read Bash(git *)"));
        assert!(joined.contains("--disallowed-tools"));
        assert!(joined.contains(" Write "));
        assert!(!joined.contains(" Read Write"), "Read must not be denied");
        assert!(joined.ends_with("--resume sid --max-turns 7 --model sonnet"));
    }

    #[test]
    fn mcp_opt_in_drops_the_strict_flag() {
        let s = spec_with("[run]\nmcp=true\ntools=[\"*\"]\n[prompt]\ninstruction=\"go\"");
        let joined = build_args(&s, "hi", None, Path::new("/p/sys.txt")).join(" ");
        assert!(!joined.contains("--strict-mcp-config"));
        assert!(joined.contains("--allowed-tools "));
        assert!(joined.contains(" Bash "));
        assert!(!joined.contains("--disallowed-tools"));
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
