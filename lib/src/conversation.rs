use crate::models::{ConversationMessage, SessionState};
use log::debug;
use serde_json::Value;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

/// Read the tail of a JSONL session log, expanding the window until it
/// contains enough context to classify session state — i.e. at least one
/// `assistant` entry — or the whole file has been read (up to a sane cap).
///
/// The 64 KiB fixed tail misbehaves when parallel tool-uses generate many
/// large `tool_result` entries: the spawning assistant `tool_use` entries
/// scroll out of view, leaving `extract_state` with no meaningful user/
/// assistant entry to judge from.
pub fn read_jsonl_tail_for_state(path: &Path) -> Vec<Value> {
    const INITIAL: u64 = 64 * 1024;
    const MAX: u64 = 4 * 1024 * 1024;

    let total_len = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return Vec::new(),
    };

    let mut window = INITIAL;
    loop {
        let entries = read_jsonl_tail(path, window);
        let has_assistant = entries
            .iter()
            .any(|e| e.get("type").and_then(|t| t.as_str()) == Some("assistant"));
        if has_assistant || window >= total_len || window >= MAX {
            debug!(
                "read_jsonl_tail_for_state: window={}B entries={} has_assistant={} total={}B",
                window,
                entries.len(),
                has_assistant,
                total_len
            );
            return entries;
        }
        window = window.saturating_mul(2);
    }
}

fn parse_jsonl_values<R: BufRead>(reader: R) -> Vec<Value> {
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<Value>(&line) {
            out.push(val);
        }
    }
    out
}

/// Read every JSONL entry in `path`, start to end. Intended for one-shot
/// review flows (e.g. Metrics → Enter on a context-growth finding); the
/// hot path should still use [`read_jsonl_tail`] / [`read_jsonl_tail_for_state`].
pub fn read_jsonl_all(path: &Path) -> Vec<Value> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    parse_jsonl_values(BufReader::new(file))
}

/// Count assistant `tool_use` blocks across an entire JSONL transcript.
///
/// Streams line-by-line and parses each line independently — never holds the
/// whole file in memory, so it stays cheap on long-running orchestrator
/// transcripts. Returns 0 if the file is missing or unreadable.
pub fn count_tool_uses(path: &Path) -> usize {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return 0,
    };
    let reader = BufReader::new(file);
    let mut count = 0usize;
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let val: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if val.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let Some(arr) = val
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            continue;
        };
        for block in arr {
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                count += 1;
            }
        }
    }
    count
}

pub fn read_jsonl_tail(path: &Path, max_bytes: u64) -> Vec<Value> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let len = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return Vec::new(),
    };

    let seek_pos = if len > max_bytes { len - max_bytes } else { 0 };
    if file.seek(SeekFrom::Start(seek_pos)).is_err() {
        return Vec::new();
    }

    let mut reader = BufReader::new(&mut file);
    if seek_pos > 0 {
        // Partial line at the seek boundary — consume and discard.
        let mut discard = String::new();
        let _ = reader.read_line(&mut discard);
    }
    parse_jsonl_values(reader)
}

pub fn read_jsonl_head(path: &Path, max_bytes: u64) -> Vec<Value> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let len = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return Vec::new(),
    };

    let read_len = len.min(max_bytes);
    let mut buf = vec![0u8; read_len as usize];
    if std::io::Read::read_exact(&mut file, &mut buf).is_err() {
        return Vec::new();
    }

    let text = String::from_utf8_lossy(&buf);
    let mut lines = Vec::new();
    let line_iter: Vec<&str> = text.lines().collect();
    let last_idx = if len > max_bytes {
        // File was truncated — discard last (potentially partial) line
        line_iter.len().saturating_sub(1)
    } else {
        line_iter.len()
    };

    for line in &line_iter[..last_idx] {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<Value>(line) {
            lines.push(val);
        }
    }

    lines
}

/// Check if a content Value (string or array) contains a `<command-name>` tag,
/// which indicates a local slash command (/clear, /compact, etc.).
fn content_contains_command_name(content: &Value) -> bool {
    if let Some(text) = content.as_str() {
        if text.contains("<command-name>") {
            return true;
        }
    }
    if let Some(arr) = content.as_array() {
        for block in arr {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    if text.contains("<command-name>") {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn is_meaningful_entry(entry: &Value) -> bool {
    // Skip metadata entries (e.g. local-command-caveat after /clear).
    if entry.get("isMeta").and_then(|v| v.as_bool()) == Some(true) {
        return false;
    }

    match entry.get("type").and_then(|t| t.as_str()) {
        Some("user") => {
            if let Some(content) = entry.get("message").and_then(|m| m.get("content")) {
                // Skip tool_result messages (auto-generated, not real user input)
                if let Some(arr) = content.as_array() {
                    let only_tool_results = arr
                        .iter()
                        .all(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"));
                    if only_tool_results && !arr.is_empty() {
                        return false;
                    }
                }
                // Skip local slash commands (/clear, /compact, etc.)
                if content_contains_command_name(content) {
                    return false;
                }
            }
            true
        }
        Some("assistant") => true,
        Some("system") => {
            // Skip local_command entries (/clear, /compact, etc.)
            let sub = entry.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
            sub != "local_command"
        }
        _ => false,
    }
}

/// Tools that block on user interaction (permission prompts, plan mode, etc.).
/// When the assistant's last action is calling one of these, the session is
/// waiting for user input, not actively processing.
const USER_INPUT_TOOLS: &[&str] = &["EnterPlanMode", "ExitPlanMode", "AskUserQuestion"];

/// Returns true if the assistant message contains a tool_use block for a tool
/// that requires user interaction.
fn assistant_awaits_user_input(entry: &Value) -> bool {
    let content = match entry
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        Some(arr) => arr,
        None => return false,
    };
    content.iter().any(|block| {
        block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
            && block
                .get("name")
                .and_then(|n| n.as_str())
                .is_some_and(|name| USER_INPUT_TOOLS.contains(&name))
    })
}

/// Returns true if there's a dangling assistant `tool_use` (no matching
/// `tool_result`) AND a `last-prompt` entry appears after it — the signature
/// of an interrupted turn where the user typed a new message. A dangling
/// `tool_use` alone can just mean the tool is still running, so we require
/// the `last-prompt` marker to disambiguate.
fn interrupted_tool_use(entries: &[Value]) -> bool {
    let mut unresolved: Vec<(usize, &str)> = Vec::new();
    let mut results: HashSet<&str> = HashSet::new();
    let mut last_prompt_idx: Option<usize> = None;

    for (i, entry) in entries.iter().enumerate() {
        let t = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if t == "last-prompt" {
            last_prompt_idx = Some(i);
            continue;
        }
        let arr = match entry
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        {
            Some(a) => a,
            None => continue,
        };
        for block in arr {
            let bt = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if t == "assistant" && bt == "tool_use" {
                if let Some(id) = block.get("id").and_then(|v| v.as_str()) {
                    unresolved.push((i, id));
                }
            } else if t == "user" && bt == "tool_result" {
                if let Some(id) = block.get("tool_use_id").and_then(|v| v.as_str()) {
                    results.insert(id);
                }
            }
        }
    }

    let Some(lp_idx) = last_prompt_idx else {
        return false;
    };
    unresolved
        .iter()
        .any(|(idx, id)| *idx < lp_idx && !results.contains(id))
}

/// Determine session state from the last meaningful user/assistant entry.
///   Processing      — last entry indicates the agent is working
///   WaitingForInput — last entry indicates a completed turn
///   Idle            — no meaningful entries (fresh session)
///
/// System entries (turn_duration, stop_hook_summary, etc.) are metadata that
/// appear between turns.  They must be skipped for state detection because
/// during an active tool-use loop a `turn_duration` entry sits between the
/// assistant's tool_use request and the tool result, causing a false
/// WaitingForInput while the agent is actually executing a tool.
pub fn extract_state(entries: &[Value]) -> SessionState {
    // Interrupted turn: dangling tool_use + trailing `last-prompt` marker
    // indicates the user hit Esc mid-tool and typed a new message — the
    // session is waiting for them to submit it.
    if interrupted_tool_use(entries) {
        debug!("extract_state: interrupted tool_use (last-prompt follows) → WaitingForInput");
        return SessionState::WaitingForInput;
    }

    let last = match entries
        .iter()
        .rev()
        .filter(|e| is_meaningful_entry(e))
        .find(|e| {
            matches!(
                e.get("type").and_then(|t| t.as_str()),
                Some("user") | Some("assistant")
            )
        }) {
        Some(e) => e,
        None => {
            debug!("extract_state: no meaningful user/assistant entry → Idle");
            return SessionState::Idle;
        }
    };

    let entry_type = last.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let state = match entry_type {
        "user" => SessionState::Processing,
        "assistant" => {
            let stop = last
                .get("message")
                .and_then(|m| m.get("stop_reason"))
                .and_then(|s| s.as_str())
                .unwrap_or("");
            match stop {
                "end_turn" => SessionState::WaitingForInput,
                // tool_use means the agent requested a tool call.
                // Some tools block on user interaction — treat those as
                // WaitingForInput so the UI shows them correctly.
                "tool_use" => {
                    let awaits = assistant_awaits_user_input(last);
                    debug!(
                        "extract_state: last=assistant stop=tool_use awaits_input={}",
                        awaits
                    );
                    if awaits {
                        SessionState::WaitingForInput
                    } else {
                        SessionState::Processing
                    }
                }
                _ => {
                    debug!(
                        "extract_state: last=assistant stop_reason={:?} → Processing",
                        stop
                    );
                    SessionState::Processing
                }
            }
        }
        _ => SessionState::Processing,
    };

    debug!("extract_state: last_type={} → {}", entry_type, state);
    state
}

/// Whether the session's most recent assistant entry ends with a `thinking`
/// block — Claude Code writes each content block as its own JSONL entry, so
/// a trailing `thinking` entry with no follow-up `text` or `tool_use` means
/// the agent is still mid-reasoning. Claude Code does not persist the
/// thinking text (only the signature), so we can only report the *fact* that
/// thinking is happening.
pub fn is_currently_thinking(entries: &[Value]) -> bool {
    entries
        .iter()
        .rev()
        .find(|e| e.get("type").and_then(|t| t.as_str()) == Some("assistant"))
        .and_then(|e| {
            let arr = e.get("message")?.get("content")?.as_array()?;
            let last_block = arr.last()?;
            Some(last_block.get("type").and_then(|t| t.as_str()) == Some("thinking"))
        })
        .unwrap_or(false)
}

/// The most recent unresolved assistant `tool_use`: the tool the agent is
/// currently executing (Processing) or the blocking tool it's waiting on
/// user input for (WaitingForInput).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentTool {
    pub name: String,
    /// Short, tool-specific input snippet (Bash command, file basename, grep
    /// pattern, …). None when the tool's input has no useful one-liner.
    pub hint: Option<String>,
}

/// Return the most recent unresolved assistant `tool_use`. Returns None if
/// every `tool_use` has a matching `tool_result`, or if the last meaningful
/// assistant turn had no tool_use.
///
/// We scan the whole window rather than just the last assistant entry so
/// parallel tool calls (multiple tool_use blocks across several assistant
/// entries with results trickling in) resolve to the outstanding one, not
/// an already-completed sibling.
pub fn extract_current_tool(entries: &[Value]) -> Option<CurrentTool> {
    let mut unresolved: Vec<(String, String, Option<String>)> = Vec::new();
    let mut results: HashSet<String> = HashSet::new();

    for entry in entries {
        let t = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let arr = match entry
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        {
            Some(a) => a,
            None => continue,
        };
        for block in arr {
            let bt = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if t == "assistant" && bt == "tool_use" {
                let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if !id.is_empty() && !name.is_empty() {
                    let hint = format_tool_hint(name, block.get("input"));
                    unresolved.push((id.to_string(), name.to_string(), hint));
                }
            } else if t == "user" && bt == "tool_result" {
                if let Some(id) = block.get("tool_use_id").and_then(|v| v.as_str()) {
                    results.insert(id.to_string());
                }
            }
        }
    }

    unresolved
        .into_iter()
        .rev()
        .find(|(id, _, _)| !results.contains(id))
        .map(|(_, name, hint)| CurrentTool { name, hint })
}

/// One-line input snippet per tool kind. Returns None when the tool's input
/// has no obvious user-facing summary, so the cell renders just the tool name.
fn format_tool_hint(name: &str, input: Option<&Value>) -> Option<String> {
    let input = input?;
    // Strip `mcp__<server>__` prefixes so `mcp__claude_ai_Notion__notion-search`
    // dispatches by its leaf (`notion-search`).
    let leaf = name.rsplit("__").next().unwrap_or(name);
    let raw = match leaf {
        "Bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        "Edit" | "Read" | "Write" | "NotebookEdit" => {
            input.get("file_path").and_then(|v| v.as_str()).map(|p| {
                Path::new(p)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(p)
                    .to_string()
            })
        }
        "Grep" | "Glob" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        "Task" => input
            .get("description")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        "WebFetch" => input
            .get("url")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        "WebSearch" => input
            .get("query")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        "TodoWrite" => input
            .get("todos")
            .and_then(|v| v.as_array())
            .map(|a| format!("{} todos", a.len())),
        _ => None,
    }?;
    let cleaned = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Sum of input + cache_read + cache_creation tokens from the *most recent*
/// assistant message — i.e. the size of the prompt that gets re-sent on the
/// next turn, which is the live context-window utilisation.
pub fn extract_context_tokens(entries: &[Value]) -> Option<u64> {
    entries
        .iter()
        .rev()
        .find(|e| e.get("type").and_then(|t| t.as_str()) == Some("assistant"))
        .and_then(|e| {
            let usage = e.get("message").and_then(|m| m.get("usage"))?;
            let f = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            let total =
                f("input_tokens") + f("cache_read_input_tokens") + f("cache_creation_input_tokens");
            if total == 0 {
                None
            } else {
                Some(total)
            }
        })
}

#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// Rule fired and chose this state. The decision-tree walk stops here.
    Decided(SessionState),
    /// Rule's preconditions matched but it didn't override anything.
    Passed,
    /// Rule's preconditions did not match.
    Skipped,
}

#[derive(Clone, Debug)]
pub struct ExplanationStep {
    pub name: &'static str,
    pub verdict: Verdict,
    pub details: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct EntrySummary {
    pub idx: usize,
    pub kind: String,
    pub timestamp: Option<String>,
    pub stop_reason: Option<String>,
    pub blocks: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct StateExplanation {
    pub final_state: SessionState,
    pub mtime_age_secs: Option<u64>,
    pub entry_count: usize,
    pub steps: Vec<ExplanationStep>,
    pub tail: Vec<EntrySummary>,
}

/// Mirror of `extract_state` (plus the `scanner.rs` Idle→Processing upgrade)
/// that records every rule's inputs and verdict for the debug popup.
pub fn explain_state(entries: &[Value], mtime_age_secs: Option<u64>) -> StateExplanation {
    let mut steps = Vec::new();

    let int_step = explain_interrupted_tool_use(entries);
    let interrupted = matches!(int_step.verdict, Verdict::Decided(_));
    steps.push(int_step);
    if interrupted {
        return finalize(
            SessionState::WaitingForInput,
            mtime_age_secs,
            entries,
            steps,
        );
    }

    let (last_step, base_state) = explain_last_meaningful(entries);
    steps.push(last_step);

    let final_state = if base_state == SessionState::Idle && mtime_age_secs.is_some_and(|s| s < 30)
    {
        steps.push(ExplanationStep {
            name: "mtime_upgrade Idle→Processing",
            verdict: Verdict::Decided(SessionState::Processing),
            details: vec![format!(
                "state is Idle but mtime age {}s < 30s → upgrade to Processing",
                mtime_age_secs.unwrap_or(0)
            )],
        });
        SessionState::Processing
    } else {
        let why = if base_state != SessionState::Idle {
            format!("state is {} (not Idle), no upgrade needed", base_state)
        } else {
            format!(
                "mtime age {} ≥ 30s threshold, no upgrade",
                mtime_age_secs.map_or("unknown".to_string(), |s| format!("{}s", s))
            )
        };
        steps.push(ExplanationStep {
            name: "mtime_upgrade Idle→Processing",
            verdict: Verdict::Skipped,
            details: vec![why],
        });
        base_state
    };

    finalize(final_state, mtime_age_secs, entries, steps)
}

fn finalize(
    final_state: SessionState,
    mtime_age_secs: Option<u64>,
    entries: &[Value],
    steps: Vec<ExplanationStep>,
) -> StateExplanation {
    StateExplanation {
        final_state,
        mtime_age_secs,
        entry_count: entries.len(),
        steps,
        tail: summarize_tail(entries, 12),
    }
}

fn explain_interrupted_tool_use(entries: &[Value]) -> ExplanationStep {
    let mut unresolved: Vec<(usize, String, Option<String>)> = Vec::new();
    let mut results: HashSet<String> = HashSet::new();
    let mut last_prompt_idx: Option<usize> = None;

    for (i, entry) in entries.iter().enumerate() {
        let t = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if t == "last-prompt" {
            last_prompt_idx = Some(i);
            continue;
        }
        let arr = match entry
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        {
            Some(a) => a,
            None => continue,
        };
        for block in arr {
            let bt = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if t == "assistant" && bt == "tool_use" {
                if let Some(id) = block.get("id").and_then(|v| v.as_str()) {
                    let name = block
                        .get("name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string());
                    unresolved.push((i, id.to_string(), name));
                }
            } else if t == "user" && bt == "tool_result" {
                if let Some(id) = block.get("tool_use_id").and_then(|v| v.as_str()) {
                    results.insert(id.to_string());
                }
            }
        }
    }

    let mut details = vec![
        format!("scanned {} entries", entries.len()),
        format!("found {} assistant tool_use blocks", unresolved.len()),
        format!("found {} user tool_result blocks", results.len()),
        format!(
            "last-prompt entry idx: {}",
            last_prompt_idx.map_or("none".to_string(), |i| i.to_string())
        ),
    ];

    let Some(lp_idx) = last_prompt_idx else {
        details.push("no last-prompt → not interrupted".into());
        return ExplanationStep {
            name: "interrupted_tool_use",
            verdict: Verdict::Skipped,
            details,
        };
    };

    let dangling: Vec<&(usize, String, Option<String>)> = unresolved
        .iter()
        .filter(|(idx, id, _)| *idx < lp_idx && !results.contains(id))
        .collect();

    if dangling.is_empty() {
        details.push(format!(
            "all tool_uses before idx {} have matching tool_results → not interrupted",
            lp_idx
        ));
        ExplanationStep {
            name: "interrupted_tool_use",
            verdict: Verdict::Passed,
            details,
        }
    } else {
        for (idx, id, name) in &dangling {
            details.push(format!(
                "  dangling tool_use idx={} name={} id={} (before last-prompt idx {})",
                idx,
                name.as_deref().unwrap_or("?"),
                short_id(id),
                lp_idx
            ));
        }
        details.push(format!(
            "{} dangling tool_use(s) before last-prompt → INTERRUPTED → WaitingForInput",
            dangling.len()
        ));
        ExplanationStep {
            name: "interrupted_tool_use",
            verdict: Verdict::Decided(SessionState::WaitingForInput),
            details,
        }
    }
}

fn explain_last_meaningful(entries: &[Value]) -> (ExplanationStep, SessionState) {
    let last = entries
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, e)| is_meaningful_entry(e))
        .find(|(_, e)| {
            matches!(
                e.get("type").and_then(|t| t.as_str()),
                Some("user") | Some("assistant")
            )
        });

    let Some((idx, last)) = last else {
        return (
            ExplanationStep {
                name: "last_meaningful_entry",
                verdict: Verdict::Decided(SessionState::Idle),
                details: vec!["no meaningful user/assistant entry → Idle".into()],
            },
            SessionState::Idle,
        );
    };

    let entry_type = last.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let ts = last
        .get("timestamp")
        .and_then(|t| t.as_str())
        .unwrap_or("?");
    let stop = last
        .get("message")
        .and_then(|m| m.get("stop_reason"))
        .and_then(|s| s.as_str())
        .unwrap_or("");

    let mut details = vec![format!(
        "selected: idx={} type={} ts={} stop_reason={:?}",
        idx, entry_type, ts, stop
    )];

    let state = match entry_type {
        "user" => {
            details.push("user message → Processing (assistant about to respond)".into());
            SessionState::Processing
        }
        "assistant" => match stop {
            "end_turn" => {
                details.push("stop_reason=end_turn → WaitingForInput".into());
                SessionState::WaitingForInput
            }
            "tool_use" => {
                let awaits = assistant_awaits_user_input(last);
                let tool_names = collect_tool_names(last);
                details.push(format!("tool_use blocks: {:?}", tool_names));
                details.push(format!(
                    "USER_INPUT_TOOLS = {:?} → awaits_user_input={}",
                    USER_INPUT_TOOLS, awaits
                ));
                if awaits {
                    details.push("blocking tool → WaitingForInput".into());
                    SessionState::WaitingForInput
                } else {
                    details.push("non-blocking tool → Processing".into());
                    SessionState::Processing
                }
            }
            _ => {
                details.push(format!("stop_reason={:?} (other) → Processing", stop));
                SessionState::Processing
            }
        },
        _ => {
            details.push(format!(
                "entry_type={:?} (unexpected) → Processing",
                entry_type
            ));
            SessionState::Processing
        }
    };

    (
        ExplanationStep {
            name: "last_meaningful_entry",
            verdict: Verdict::Decided(state.clone()),
            details,
        },
        state,
    )
}

fn collect_tool_names(entry: &Value) -> Vec<String> {
    let arr = match entry
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        .filter_map(|b| b.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect()
}

fn summarize_tail(entries: &[Value], n: usize) -> Vec<EntrySummary> {
    let start = entries.len().saturating_sub(n);
    entries
        .iter()
        .enumerate()
        .skip(start)
        .map(|(idx, e)| {
            let kind = match e.get("type").and_then(|t| t.as_str()).unwrap_or("?") {
                "system" => format!(
                    "system:{}",
                    e.get("subtype").and_then(|s| s.as_str()).unwrap_or("?")
                ),
                t => t.to_string(),
            };
            let timestamp = e
                .get("timestamp")
                .and_then(|t| t.as_str())
                .map(|s| s.get(11..23).unwrap_or(s).to_string());
            let stop_reason = e
                .get("message")
                .and_then(|m| m.get("stop_reason"))
                .and_then(|s| s.as_str())
                .map(String::from);
            let blocks = e
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|b| {
                            let t = b.get("type").and_then(|t| t.as_str()).unwrap_or("?");
                            match t {
                                "tool_use" => format!(
                                    "tool_use({})",
                                    b.get("name").and_then(|n| n.as_str()).unwrap_or("?")
                                ),
                                "tool_result" => format!(
                                    "tool_result({})",
                                    b.get("tool_use_id")
                                        .and_then(|v| v.as_str())
                                        .map(short_id)
                                        .unwrap_or_else(|| "?".into())
                                ),
                                _ => t.to_string(),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            EntrySummary {
                idx,
                kind,
                timestamp,
                stop_reason,
                blocks,
            }
        })
        .collect()
}

fn short_id(id: &str) -> String {
    let mut s: String = id.chars().take(14).collect();
    if id.chars().count() > 14 {
        s.push('…');
    }
    s
}

pub fn extract_last_user_message(entries: &[Value]) -> Option<String> {
    entries
        .iter()
        .rev()
        .filter(|e| is_meaningful_entry(e))
        .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("user"))
        .find_map(|e| extract_user_text(e, 120))
}

pub fn extract_first_user_message(entries: &[Value]) -> Option<String> {
    entries
        .iter()
        .filter(|e| is_meaningful_entry(e))
        .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("user"))
        .find_map(|e| extract_user_text(e, 200))
}

/// Extract text from a user message entry, handling both string and array content.
fn extract_user_text(entry: &Value, max_len: usize) -> Option<String> {
    let content = entry.get("message")?.get("content")?;
    if let Some(text) = content.as_str() {
        if !text.is_empty() {
            return Some(truncate_str(text, max_len));
        }
    }
    if let Some(arr) = content.as_array() {
        for block in arr {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    if !text.is_empty() {
                        return Some(truncate_str(text, max_len));
                    }
                }
            }
        }
    }
    None
}

pub fn extract_metadata(entries: &[Value]) -> (Option<String>, Option<String>, Option<String>) {
    let mut git_branch = None;
    let mut model = None;
    let mut version = None;

    for entry in entries.iter().rev() {
        if git_branch.is_none() {
            if let Some(b) = entry.get("gitBranch").and_then(|v| v.as_str()) {
                git_branch = Some(b.to_string());
            }
        }
        if model.is_none() {
            if let Some(m) = entry
                .get("message")
                .and_then(|msg| msg.get("model"))
                .and_then(|v| v.as_str())
            {
                model = Some(m.to_string());
            }
        }
        if version.is_none() {
            if let Some(v) = entry.get("version").and_then(|v| v.as_str()) {
                version = Some(v.to_string());
            }
        }
        if git_branch.is_some() && model.is_some() && version.is_some() {
            break;
        }
    }

    (git_branch, model, version)
}

pub fn extract_last_activity(entries: &[Value]) -> Option<u64> {
    entries
        .iter()
        .rev()
        .find_map(|e| e.get("timestamp").and_then(parse_timestamp_ms))
}

/// Parse a JSONL timestamp field to epoch milliseconds.
/// Handles both integer timestamps and ISO 8601 strings (e.g. "2026-04-15T18:14:30.201Z").
pub fn parse_timestamp_ms(val: &Value) -> Option<u64> {
    if let Some(n) = val.as_u64() {
        return Some(n);
    }
    if let Some(s) = val.as_str() {
        return parse_iso8601_ms(s);
    }
    None
}

fn parse_iso8601_ms(s: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis() as u64)
}

pub fn extract_messages(entries: &[Value], count: usize) -> Vec<ConversationMessage> {
    let mut msgs: Vec<ConversationMessage> = entries
        .iter()
        .rev()
        .filter(|e| is_meaningful_entry(e))
        .take(count)
        .map(|e| {
            let role = e
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("unknown")
                .to_string();

            let content_preview = extract_text_content(e);
            let timestamp = e.get("timestamp").and_then(parse_timestamp_ms).unwrap_or(0);

            let model = e
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let stop_reason = e
                .get("message")
                .and_then(|m| m.get("stop_reason"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let usage_u64 = |field: &str| -> Option<u64> {
                e.get("message")
                    .and_then(|m| m.get("usage"))
                    .and_then(|u| u.get(field))
                    .and_then(|v| v.as_u64())
            };
            let input_tokens = usage_u64("input_tokens");
            let output_tokens = usage_u64("output_tokens");
            let cache_read_input_tokens = usage_u64("cache_read_input_tokens");
            let cache_creation_input_tokens = usage_u64("cache_creation_input_tokens");

            ConversationMessage {
                role,
                content_preview,
                timestamp,
                model,
                stop_reason,
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
            }
        })
        .collect::<Vec<_>>();
    msgs.reverse();
    msgs
}

pub fn extract_token_totals(entries: &[Value]) -> (u64, u64) {
    let mut total_input = 0u64;
    let mut total_output = 0u64;

    for entry in entries {
        if let Some(usage) = entry.get("message").and_then(|m| m.get("usage")) {
            if let Some(input) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                total_input += input;
            }
            if let Some(cache_create) = usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
            {
                total_input += cache_create;
            }
            if let Some(cache_read) = usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
            {
                total_input += cache_read;
            }
            if let Some(output) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                total_output += output;
            }
        }
    }

    (total_input, total_output)
}

pub(crate) const NO_CONTENT: &str = "(no content)";
pub(crate) const NO_TEXT_CONTENT: &str = "(no text content)";
pub(crate) const TOOL_MARKER_PREFIX: &str = "[tool: ";
pub(crate) const THINKING_MARKER: &str = "[thinking...]";

fn extract_text_content(entry: &Value) -> String {
    let msg_type = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match msg_type {
        "user" => {
            if let Some(content) = entry.get("message").and_then(|m| m.get("content")) {
                if let Some(text) = content.as_str() {
                    return truncate_str(text, 200);
                }
                if let Some(arr) = content.as_array() {
                    // Look for text blocks first
                    for block in arr {
                        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                return truncate_str(text, 200);
                            }
                        }
                    }
                    // If only tool_result blocks, summarize them
                    let has_tool_results = arr
                        .iter()
                        .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"));
                    if has_tool_results {
                        return "(tool result)".to_string();
                    }
                    return "(complex content)".to_string();
                }
            }
            NO_CONTENT.to_string()
        }
        "assistant" => {
            if let Some(content) = entry.get("message").and_then(|m| m.get("content")) {
                if let Some(arr) = content.as_array() {
                    let mut parts = Vec::new();
                    for block in arr {
                        match block.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    parts.push(truncate_str(text, 200));
                                }
                            }
                            Some("tool_use") => {
                                parts.push(format!(
                                    "{}{}]",
                                    TOOL_MARKER_PREFIX,
                                    tool_display(block)
                                ));
                            }
                            Some("thinking") => {
                                parts.push(THINKING_MARKER.to_string());
                            }
                            _ => {}
                        }
                    }
                    if parts.is_empty() {
                        return NO_TEXT_CONTENT.to_string();
                    }
                    return parts.join(" ");
                }
                if let Some(text) = content.as_str() {
                    return truncate_str(text, 200);
                }
            }
            NO_CONTENT.to_string()
        }
        "system" => {
            let subtype = entry
                .get("subtype")
                .and_then(|s| s.as_str())
                .unwrap_or("system");
            format!("[{}]", subtype)
        }
        _ => "(unknown)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_iso8601_with_millis() {
        let ms = parse_iso8601_ms("2026-04-15T18:14:30.201Z").unwrap();
        // 2026-04-15T18:14:30.201Z
        assert_eq!(ms % 1000, 201);
        assert!(ms > 1_776_000_000_000); // sanity: after 2026
    }

    #[test]
    fn parse_iso8601_no_millis() {
        let ms = parse_iso8601_ms("2026-04-15T18:14:30Z").unwrap();
        assert_eq!(ms % 1000, 0);
    }

    #[test]
    fn parse_timestamp_ms_integer() {
        let val = serde_json::json!(1776271524302u64);
        assert_eq!(parse_timestamp_ms(&val), Some(1776271524302));
    }

    #[test]
    fn parse_timestamp_ms_string() {
        let val = serde_json::json!("2026-04-15T18:14:30.201Z");
        let ms = parse_timestamp_ms(&val).unwrap();
        assert_eq!(ms % 1000, 201);
    }

    #[test]
    fn explain_state_flags_parallel_agent_false_positive() {
        // Reproduces the bug: 3 parallel Agent tool_uses, only 1 resolved,
        // and a `last-prompt` entry sits between the resolved ones — current
        // `interrupted_tool_use` fires and (incorrectly) returns WaitingForInput.
        // The explanation should make the reason explicit.
        let entries = vec![
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-04-16T17:29:42.000Z",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [{"type": "tool_use", "id": "agent_a", "name": "Agent", "input": {}}]}
            }),
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-04-16T17:29:49.000Z",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [{"type": "tool_use", "id": "agent_b", "name": "Agent", "input": {}}]}
            }),
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-04-16T17:29:58.000Z",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [{"type": "tool_use", "id": "agent_c", "name": "Agent", "input": {}}]}
            }),
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-04-16T17:30:25.000Z",
                "message": {"role": "user",
                    "content": [{"type": "tool_result", "tool_use_id": "agent_a"}]}
            }),
            serde_json::json!({"type": "last-prompt", "lastPrompt": "earlier user prompt"}),
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-04-16T17:30:31.000Z",
                "message": {"role": "user",
                    "content": [{"type": "tool_result", "tool_use_id": "agent_b"}]}
            }),
        ];

        let exp = explain_state(&entries, Some(5));
        assert_eq!(exp.final_state, SessionState::WaitingForInput);

        let int_step = exp
            .steps
            .iter()
            .find(|s| s.name == "interrupted_tool_use")
            .expect("interrupted_tool_use step present");
        assert_eq!(
            int_step.verdict,
            Verdict::Decided(SessionState::WaitingForInput),
            "current heuristic still misfires here — that's the bug"
        );
        assert!(
            int_step.details.iter().any(|d| d.contains("agent_c")),
            "explanation should name the dangling tool_use that triggered the verdict"
        );
    }

    #[test]
    fn read_jsonl_tail_for_state_expands_past_64k_of_tool_results() {
        use std::io::Write;
        let tmp =
            std::env::temp_dir().join(format!("cc_hub_expand_test_{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&tmp);

        let mut f = std::fs::File::create(&tmp).expect("create tmp");
        // Assistant launches 2 parallel agents at the top of the file.
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"role":"assistant","stop_reason":"tool_use","content":[{{"type":"tool_use","id":"agent_a","name":"Agent","input":{{}}}},{{"type":"tool_use","id":"agent_b","name":"Agent","input":{{}}}}]}}}}"#
        ).unwrap();
        // Pad with ~120 KiB of fat tool_result entries so the spawning
        // assistant entry falls outside a single 64 KiB tail window.
        let fat_payload: String = "x".repeat(2000);
        for i in 0..60 {
            writeln!(
                f,
                r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"pad{}","content":"{}"}}]}}}}"#,
                i, fat_payload
            )
            .unwrap();
        }
        // Final tool_result is one of the sibling agents finishing.
        writeln!(
            f,
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"agent_a"}}]}}}}"#
        ).unwrap();
        drop(f);

        // Fixed 64 KiB tail would miss the assistant entry entirely.
        let fixed = read_jsonl_tail(&tmp, 65536);
        assert!(
            !fixed
                .iter()
                .any(|e| e.get("type").and_then(|t| t.as_str()) == Some("assistant")),
            "precondition: fixed 64 KiB tail must not contain the spawning assistant entry"
        );

        // Expanding reader should pull it in.
        let expanded = read_jsonl_tail_for_state(&tmp);
        assert!(
            expanded
                .iter()
                .any(|e| e.get("type").and_then(|t| t.as_str()) == Some("assistant")),
            "expanding reader should surface the assistant entry"
        );

        // And the state resolves to Processing (unresolved agent_b tool_use
        // means siblings are still in flight).
        let state = extract_state(&expanded);
        assert_ne!(state, SessionState::WaitingForInput);

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn extract_last_activity_with_iso_timestamps() {
        let entries = vec![
            serde_json::json!({"type": "user", "timestamp": "2026-04-15T18:14:00.000Z"}),
            serde_json::json!({"type": "assistant", "timestamp": "2026-04-15T18:14:30.201Z"}),
        ];
        let result = extract_last_activity(&entries);
        assert!(result.is_some());
        assert_eq!(result.unwrap() % 1000, 201);
    }

    #[test]
    fn extract_current_tool_returns_unresolved_among_parallel_calls() {
        // Two parallel Bash + Edit tool_uses, only the Bash one resolved.
        let entries = vec![
            serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [
                        {"type": "tool_use", "id": "t1", "name": "Bash", "input": {}},
                        {"type": "tool_use", "id": "t2", "name": "Edit", "input": {}}
                    ]}
            }),
            serde_json::json!({
                "type": "user",
                "message": {"role": "user",
                    "content": [{"type": "tool_result", "tool_use_id": "t1"}]}
            }),
        ];
        let got = extract_current_tool(&entries).unwrap();
        assert_eq!(got.name, "Edit");
    }

    #[test]
    fn extract_current_tool_none_when_all_resolved() {
        let entries = vec![
            serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "stop_reason": "end_turn",
                    "content": [{"type": "tool_use", "id": "t1", "name": "Read", "input": {}}]}
            }),
            serde_json::json!({
                "type": "user",
                "message": {"role": "user",
                    "content": [{"type": "tool_result", "tool_use_id": "t1"}]}
            }),
        ];
        assert_eq!(extract_current_tool(&entries), None);
    }

    #[test]
    fn extract_current_tool_includes_bash_command_hint() {
        let entries = vec![serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "stop_reason": "tool_use",
                "content": [{"type": "tool_use", "id": "t1", "name": "Bash",
                    "input": {"command": "cargo build --release"}}]}
        })];
        let got = extract_current_tool(&entries).unwrap();
        assert_eq!(got.name, "Bash");
        assert_eq!(got.hint.as_deref(), Some("cargo build --release"));
    }

    #[test]
    fn extract_current_tool_edit_hint_is_basename() {
        let entries = vec![serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "stop_reason": "tool_use",
                "content": [{"type": "tool_use", "id": "t1", "name": "Edit",
                    "input": {"file_path": "/home/u/proj/src/main.rs"}}]}
        })];
        let got = extract_current_tool(&entries).unwrap();
        assert_eq!(got.hint.as_deref(), Some("main.rs"));
    }

    #[test]
    fn extract_current_tool_unknown_tool_has_no_hint() {
        let entries = vec![serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "stop_reason": "tool_use",
                "content": [{"type": "tool_use", "id": "t1", "name": "MysteryTool",
                    "input": {"foo": "bar"}}]}
        })];
        let got = extract_current_tool(&entries).unwrap();
        assert_eq!(got.hint, None);
    }

    #[test]
    fn extract_context_tokens_sums_input_and_cache() {
        let entries = vec![serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "usage": {
                "input_tokens": 1000,
                "cache_read_input_tokens": 50000,
                "cache_creation_input_tokens": 4000,
                "output_tokens": 200
            }}
        })];
        assert_eq!(extract_context_tokens(&entries), Some(55000));
    }

    #[test]
    fn extract_context_tokens_none_when_no_assistant_entry() {
        let entries = vec![serde_json::json!({"type": "user", "message": {"role": "user"}})];
        assert_eq!(extract_context_tokens(&entries), None);
    }

    #[test]
    fn is_currently_thinking_true_when_last_assistant_block_is_thinking() {
        // Claude Code writes each content block as its own JSONL entry — a
        // trailing thinking entry with no text/tool_use follow-up means the
        // agent is still mid-reasoning.
        let entries = vec![
            serde_json::json!({"type": "user", "message": {"role": "user", "content": "hi"}}),
            serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [{"type": "thinking", "thinking": ""}]}
            }),
        ];
        assert!(is_currently_thinking(&entries));
    }

    #[test]
    fn is_currently_thinking_false_when_tool_use_follows() {
        let entries = vec![
            serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [{"type": "thinking", "thinking": ""}]}
            }),
            serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}]}
            }),
        ];
        assert!(!is_currently_thinking(&entries));
    }

    #[test]
    fn is_currently_thinking_false_when_no_assistant_entry() {
        let entries =
            vec![serde_json::json!({"type": "user", "message": {"role": "user", "content": "hi"}})];
        assert!(!is_currently_thinking(&entries));
    }

    #[test]
    fn extract_current_tool_prefers_most_recent_unresolved() {
        // Two assistant turns with unresolved tool_uses; the newer one wins.
        let entries = vec![
            serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [{"type": "tool_use", "id": "old", "name": "Bash", "input": {}}]}
            }),
            serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [{"type": "tool_use", "id": "new", "name": "Grep", "input": {}}]}
            }),
        ];
        let got = extract_current_tool(&entries).unwrap();
        assert_eq!(got.name, "Grep");
    }
}

/// Strip XML-like tags (e.g. `<bash-stdout>`, `<system-reminder>`) that leak
/// from Claude Code's internal JSONL format.
fn strip_xml_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            // Consume everything up to and including the closing '>'
            let mut found_close = false;
            for inner in chars.by_ref() {
                if inner == '>' {
                    found_close = true;
                    break;
                }
            }
            if !found_close {
                // Malformed — put the '<' back
                out.push('<');
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn tool_display(block: &serde_json::Value) -> String {
    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("?");
    let Some(raw) = block.get("input").and_then(|i| tool_brief_arg(name, i)) else {
        return name.to_string();
    };
    // `]` would terminate the surrounding `[tool: ...]` marker; whitespace
    // collapses to single spaces so the display fits on one line.
    let cleaned = raw.replace(']', ")");
    let brief: String = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut end = brief.len().min(60);
    while end > 0 && !brief.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        name.to_string()
    } else if end < brief.len() {
        format!("{}({}…)", name, &brief[..end])
    } else {
        format!("{}({})", name, brief)
    }
}

fn tool_brief_arg(name: &str, input: &serde_json::Value) -> Option<String> {
    let s = |key: &str| {
        input
            .get(key)
            .and_then(|v| v.as_str())
            .map(|v| v.to_string())
    };
    match name {
        "Bash" => s("command"),
        "Read" | "Edit" | "Write" | "NotebookEdit" | "MultiEdit" => s("file_path"),
        "Glob" => s("pattern"),
        "Grep" => {
            let pat = s("pattern")?;
            match s("glob") {
                Some(g) if !g.is_empty() => Some(format!("{} in {}", pat, g)),
                _ => Some(pat),
            }
        }
        "WebFetch" => s("url"),
        "WebSearch" => s("query"),
        "Task" | "Agent" => s("description").or_else(|| s("subagent_type")),
        "TodoWrite" | "TaskCreate" | "TaskUpdate" => s("title").or_else(|| s("description")),
        _ => {
            // Generic fallback: first string-valued field in the input object.
            input
                .as_object()
                .and_then(|obj| obj.values().find_map(|v| v.as_str().map(String::from)))
        }
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    let s = strip_xml_tags(s);
    let s = s.trim();
    let first_line = s.lines().next().unwrap_or(s);
    if first_line.len() <= max {
        first_line.to_string()
    } else {
        // Find a char boundary at or before `max` to avoid splitting multi-byte chars.
        let mut end = max.min(first_line.len());
        while end > 0 && !first_line.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &first_line[..end])
    }
}
