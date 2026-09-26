//! Test fixtures shared across the binary: the env lock, a $HOME sandbox,
//! and argv construction.

/// `HOME` and `CLAUDE_CONFIG_DIR` are process-global, so every test in this
/// binary that mutates them must hold this one lock. Per-module locks let
/// those tests race and cascade `PoisonError` under `cargo test --workspace`.
pub(crate) static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn with_tempdir_home<F: FnOnce()>(f: F) {
    // $HOME / CLAUDE_CONFIG_DIR are process-global; serialise on the
    // crate-wide lock so these tests don't race env-mutating tests in
    // other modules (e.g. `extract_claude_config_dir`'s).
    let _g = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().expect("tempdir");
    let prev = std::env::var_os("HOME");
    std::env::set_var("HOME", home.path());
    f();
    match prev {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

pub(crate) fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}
