/// Pull a global `--claude-config-dir <path>` (or `--claude-config-dir=<path>`)
/// out of argv and export it as `CLAUDE_CONFIG_DIR` for this process. Setting
/// the env var — rather than threading a value through — means the directly
/// spawned `claude -p` helpers (titles) inherit the right
/// account for free; mux-spawned sessions get it re-applied explicitly in
/// `spawn`. Returns argv with the flag (and its value) removed so per-verb flag
/// parsers don't choke on it.
pub(crate) fn extract_claude_config_dir(argv: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len());
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        let dir = if let Some(v) = a.strip_prefix("--claude-config-dir=") {
            Some(v.to_string())
        } else if a == "--claude-config-dir" {
            let v = argv.get(i + 1).cloned();
            i += 1; // also skip the value
            v
        } else {
            out.push(a.clone());
            i += 1;
            continue;
        };
        if let Some(d) = dir {
            std::env::set_var("CLAUDE_CONFIG_DIR", expand_tilde(&d));
        }
        i += 1;
    }
    out
}

/// Expand a leading `~/` (or bare `~`) to `$HOME`. Shells already do this for
/// unquoted args, but a quoted/scripted value reaches us literally.
fn expand_tilde(path: &str) -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return path.to_string();
    };
    let home = home.to_string_lossy();
    if path == "~" {
        return home.into_owned();
    }
    match path.strip_prefix("~/") {
        Some(rest) => format!("{}/{}", home.trim_end_matches('/'), rest),
        None => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::argv;

    fn with_clean_env<F: FnOnce()>(f: F) {
        let _guard = crate::test_util::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev_cfg = std::env::var_os("CLAUDE_CONFIG_DIR");
        let prev_home = std::env::var_os("HOME");
        std::env::remove_var("CLAUDE_CONFIG_DIR");
        f();
        match prev_cfg {
            Some(v) => std::env::set_var("CLAUDE_CONFIG_DIR", v),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn strips_space_separated_flag_and_sets_env() {
        with_clean_env(|| {
            let rest = extract_claude_config_dir(argv(&[
                "--claude-config-dir",
                "/tmp/acct",
                "spawn-worker",
                "--task",
                "t1",
            ]));
            assert_eq!(rest, argv(&["spawn-worker", "--task", "t1"]));
            assert_eq!(std::env::var("CLAUDE_CONFIG_DIR").unwrap(), "/tmp/acct");
        });
    }

    #[test]
    fn strips_equals_form_flag() {
        with_clean_env(|| {
            let rest = extract_claude_config_dir(argv(&["--claude-config-dir=/tmp/b", "task"]));
            assert_eq!(rest, argv(&["task"]));
            assert_eq!(std::env::var("CLAUDE_CONFIG_DIR").unwrap(), "/tmp/b");
        });
    }

    #[test]
    fn no_flag_leaves_argv_and_env_untouched() {
        with_clean_env(|| {
            let rest = extract_claude_config_dir(argv(&["worker", "list"]));
            assert_eq!(rest, argv(&["worker", "list"]));
            assert!(std::env::var_os("CLAUDE_CONFIG_DIR").is_none());
        });
    }

    #[test]
    fn expands_leading_tilde() {
        with_clean_env(|| {
            std::env::set_var("HOME", "/home/example");
            extract_claude_config_dir(argv(&["--claude-config-dir", "~/.claude-personal"]));
            assert_eq!(
                std::env::var("CLAUDE_CONFIG_DIR").unwrap(),
                "/home/example/.claude-personal"
            );
        });
    }
}
