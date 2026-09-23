//! Shared fixture for CLI verb tests: a serialised $HOME sandbox.

pub(crate) fn with_tempdir_home<F: FnOnce()>(f: F) {
    // $HOME / CLAUDE_CONFIG_DIR are process-global; serialise on the
    // crate-wide lock so these tests don't race env-mutating tests in
    // other modules (e.g. `extract_claude_config_dir`'s).
    let _g = crate::ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().expect("tempdir");
    let prev = std::env::var_os("HOME");
    std::env::set_var("HOME", home.path());
    f();
    match prev {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}
