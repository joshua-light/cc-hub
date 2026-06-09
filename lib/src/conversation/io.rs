//! JSONL reading and streaming block counters.

use log::debug;
use serde_json::Value;
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
///
/// NOTE: each doubling seeks to `len - window` and re-reads the whole (larger)
/// window from scratch, so a file that needs several doublings re-reads the
/// already-seen suffix each round. We deliberately leave this as-is: the
/// re-read sits behind [`derive_state_cached`], which only invokes it on an
/// mtime change, and resuming a partial read across windows is fiddly given the
/// partial-line discard at each seek boundary. Most transcripts resolve on the
/// first 64 KiB window anyway.
///
/// [`derive_state_cached`]: super::derive_state_cached
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
    count_tool_uses_in_reader(BufReader::new(file))
}

/// Streaming counter for assistant `tool_use` blocks reading from any
/// `BufRead`. Shared with the incremental cache in
/// [`crate::tool_use_count`], which seeks to a previously-known offset and
/// counts only the suffix.
pub fn count_tool_uses_in_reader<R: BufRead>(reader: R) -> usize {
    count_blocks_in_reader(reader, |val| {
        if val.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            return 0;
        }
        count_blocks_of_type(val, "tool_use")
    })
}

/// Streaming dispatch over JSONL entries. For each well-formed line, calls
/// `per_entry` and sums its returns. Skips empty lines and parse errors.
/// Shared between Claude and Pi tool-use counters and the incremental cache.
pub fn count_blocks_in_reader<R, F>(reader: R, mut per_entry: F) -> usize
where
    R: BufRead,
    F: FnMut(&Value) -> usize,
{
    let mut total = 0usize;
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(val): Result<Value, _> = serde_json::from_str(&line) else {
            continue;
        };
        total += per_entry(&val);
    }
    total
}

/// Count blocks of the given `block_type` in `entry.message.content`. Used
/// by both Claude (`tool_use`) and Pi (`toolCall`) counters once the caller
/// has confirmed the entry is an assistant message.
pub fn count_blocks_of_type(entry: &Value, block_type: &str) -> usize {
    let Some(arr) = entry
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    else {
        return 0;
    };
    arr.iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some(block_type))
        .count()
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

    let seek_pos = len.saturating_sub(max_bytes);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::extract_state;
    use crate::models::SessionState;

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
}
