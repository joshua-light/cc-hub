//! Shared test helpers for the conversation cache tests.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Serializes the parse-counter-sensitive cache tests: the counter is
/// process-global, so two tests parsing concurrently would race the
/// "must NOT advance" assertions.
pub(crate) static PARSE_COUNTER_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn write_jsonl(path: &Path, lines: &[&str]) {
    let mut f = std::fs::File::create(path).expect("create");
    for line in lines {
        writeln!(f, "{}", line).expect("write");
    }
}

pub(crate) fn fresh_jsonl(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "cchub-state-cache-{}-{}.jsonl",
        std::process::id(),
        name
    ));
    let _ = std::fs::remove_file(&p);
    p
}
