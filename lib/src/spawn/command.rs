//! The shell command line that launches an agent, per backend.

use super::SessionTarget;
use crate::agent::{AgentConfig, AgentKind};
use crate::config;
use crate::platform::paths;
use crate::platform::terminal::shell_quote;
use crate::sessions::pi_bridge;
use std::io;

/// Prefix `cmd` with an environment-variable assignment the target shell can
/// parse. On Unix the command runs through a POSIX shell (`$SHELL -ic`, see
/// [`crate::platform::mux::spawn_detached`]); on Windows it lands in pwsh.
/// `VAR=value cmd` is POSIX-only — pwsh reads it as an argument, not an
/// assignment, so the agent never starts.
///
/// Unix keeps the exact `VAR='value' cmd` form (POSIX single-quote escaping via
/// [`shell_quote`]); Windows emits `$env:VAR = 'value'; cmd` with pwsh
/// single-quote-literal escaping (double any embedded single quote). Pure and
/// `windows`-parameterised so both branches are unit-testable on any host — we
/// can't run pwsh in CI.
fn prefix_env(var: &str, value: &str, cmd: &str, windows: bool) -> String {
    if windows {
        let escaped = value.replace('\'', "''");
        format!("$env:{} = '{}'; {}", var, escaped, cmd)
    } else {
        format!("{}={} {}", var, shell_quote(value), cmd)
    }
}

pub(super) fn build_agent_command(
    agent: &AgentConfig,
    tmux_name: &str,
    target: Option<SessionTarget>,
    initial_prompt: Option<&str>,
    model: Option<&str>,
    readonly_tools: bool,
) -> io::Result<String> {
    let mut cmd = agent.command.clone();
    let account = crate::resources::for_agent(&agent.id);
    let configured = config::get().agents.get(&agent.id);
    if configured.and_then(|a| a.account.as_ref()).is_some() && account.is_none() {
        return Err(io::Error::other(
            "configured account does not exist in resources.toml",
        ));
    }
    if let Some(account) = &account {
        if account.provider != agent.kind {
            return Err(io::Error::other("account/provider mismatch"));
        }
    }
    if let Some(effort) = configured.and_then(|a| a.effort.as_deref()) {
        match agent.kind {
            AgentKind::Claude => {
                cmd.push_str(" --effort ");
                cmd.push_str(&shell_quote(effort));
            }
            AgentKind::Codex => {
                cmd.push_str(" -c ");
                cmd.push_str(&shell_quote(&format!("model_reasoning_effort={effort:?}")));
            }
            AgentKind::Pi => {}
        }
    }

    match agent.kind {
        AgentKind::Claude => {
            if let Some(model) = model {
                cmd.push_str(" --model ");
                cmd.push_str(&shell_quote(model));
            }
            match target {
                Some(SessionTarget::Resume(sid)) => {
                    cmd.push_str(" --resume ");
                    cmd.push_str(&shell_quote(&sid));
                }
                Some(SessionTarget::ResumeFile(path)) => {
                    return Err(io::Error::other(format!(
                        "claude backend cannot resume by session file: {}",
                        path.display()
                    )));
                }
                Some(SessionTarget::Fresh(sid)) => {
                    cmd.push_str(" --session-id ");
                    cmd.push_str(&shell_quote(&sid));
                }
                None => {}
            }
            // Positional prompt, valid bare and after `--resume` — how the
            // resource broker opens replacement sessions. Callers that queue
            // prompts via send-keys gate on `supports_initial_prompt()` and
            // pass None here, so this only fires for explicit openers
            // (respawn's continuation note).
            if let Some(prompt) = initial_prompt {
                cmd.push(' ');
                cmd.push_str(&shell_quote(prompt));
            }
            // Pin the spawned `claude` to this instance's account. The hub
            // process already has CLAUDE_CONFIG_DIR set, but a detached mux
            // session attaches to a possibly-pre-existing tmux server whose
            // captured environment may not include it — so set it explicitly.
            if let Some(dir) = paths::claude_config_dir().filter(|_| account.is_none()) {
                cmd = prefix_env(
                    "CLAUDE_CONFIG_DIR",
                    &dir.to_string_lossy(),
                    &cmd,
                    cfg!(windows),
                );
            }
        }
        AgentKind::Pi => {
            if let Some(model) = model {
                cmd.push_str(" --model ");
                cmd.push_str(&shell_quote(model));
            }
            if agent.use_bridge {
                let bridge = pi_bridge::ensure_bridge_file()?;
                cmd.push_str(" -e ");
                cmd.push_str(&shell_quote(&bridge.to_string_lossy()));
            }
            if readonly_tools {
                cmd.push_str(" --tools read,grep,find,ls");
            }
            match target {
                Some(SessionTarget::Resume(sid)) => {
                    cmd.push_str(" --session ");
                    cmd.push_str(&shell_quote(&sid));
                }
                Some(SessionTarget::ResumeFile(path)) => {
                    cmd.push_str(" --session ");
                    cmd.push_str(&shell_quote(&path.to_string_lossy()));
                }
                Some(SessionTarget::Fresh(sid)) => {
                    return Err(io::Error::other(format!(
                        "pi backend cannot start under a chosen session id: {}",
                        sid
                    )));
                }
                None => {}
            }
            if let Some(prompt) = initial_prompt {
                cmd.push(' ');
                cmd.push_str(&shell_quote(prompt));
            }
            if agent.use_bridge {
                let heartbeat_dir = paths::pi_heartbeats_dir()
                    .ok_or_else(|| io::Error::other("home dir unavailable for pi heartbeat dir"))?;
                std::fs::create_dir_all(&heartbeat_dir)?;
                cmd = format!(
                    "CC_HUB_TMUX={} CC_HUB_AGENT_ID={} CC_HUB_HEARTBEAT_DIR={} {}",
                    shell_quote(tmux_name),
                    shell_quote(&agent.id),
                    shell_quote(&heartbeat_dir.to_string_lossy()),
                    cmd
                );
            }
        }
        AgentKind::Codex => {
            // Codex's CLI shape differs from Claude/Pi: `-m`/`-c` are top-level
            // options that precede the (optional) `resume <uuid>` subcommand,
            // and a resumed session restores its own model — so `-m` applies to
            // fresh sessions only. Any fixed flags (e.g. reasoning effort) live
            // in `agent.command` from config and are already in `cmd`.
            match target {
                Some(SessionTarget::Resume(sid)) => {
                    cmd.push_str(" resume ");
                    cmd.push_str(&shell_quote(&sid));
                }
                Some(SessionTarget::ResumeFile(path)) => {
                    return Err(io::Error::other(format!(
                        "codex backend resumes by session id, not file: {}",
                        path.display()
                    )));
                }
                Some(SessionTarget::Fresh(sid)) => {
                    return Err(io::Error::other(format!(
                        "codex backend cannot start under a chosen session id: {}",
                        sid
                    )));
                }
                None => {
                    if let Some(model) = model {
                        cmd.push_str(" -m ");
                        cmd.push_str(&shell_quote(model));
                    }
                }
            }
            // Codex takes the initial prompt as a positional `[PROMPT]`, valid
            // both bare and after `resume <uuid>`.
            if let Some(prompt) = initial_prompt {
                cmd.push(' ');
                cmd.push_str(&shell_quote(prompt));
            }
        }
    }

    if let Some(account) = account {
        if cfg!(windows) {
            return Err(io::Error::other(
                "named subscription accounts currently require Unix",
            ));
        }
        let mut env = std::process::Command::new("env");
        account.apply(&mut env);
        let unset: Vec<String> = env
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect();
        let set: Vec<(String, String)> = env
            .get_envs()
            .filter_map(|(key, value)| {
                value.map(|v| {
                    (
                        key.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned(),
                    )
                })
            })
            .collect();
        cmd = account_env_prefix(&unset, &set, &cmd);
    }
    Ok(cmd)
}

/// Scope a subscription account's environment onto `cmd` as plain POSIX shell
/// syntax: `unset A B; export K='v'; cmd`. The string is parsed by the user's
/// interactive shell (`$SHELL -ic`, see
/// [`crate::platform::mux::spawn_detached`]), so `cmd` stays at command
/// position and rc aliases such as `cc-hub-new` still expand. The earlier
/// `env -u A ... /bin/sh -c 'cmd'` form re-parsed `cmd` in a bare `/bin/sh`
/// that reads no rc file: `cc-hub-new: command not found`, and the hub
/// reported the session as "exited during startup".
///
/// `export` rather than a `K='v' cmd` prefix: a prefix scopes to the first
/// simple command only, so a compound `a && b` would run `b` under the wrong
/// account. The statements must stay inside the `-ic` shell, after its rc has
/// run; put in front of `$SHELL`, an rc that exports `CLAUDE_CONFIG_DIR` or
/// an API key would undo the switch. Needs a POSIX shell: fish has neither
/// `unset` nor `export`.
fn account_env_prefix(unset: &[String], set: &[(String, String)], cmd: &str) -> String {
    let mut out = String::new();
    if !unset.is_empty() {
        out.push_str("unset");
        for key in unset {
            out.push(' ');
            out.push_str(key);
        }
        out.push_str("; ");
    }
    for (key, value) in set {
        out.push_str("export ");
        out.push_str(key);
        out.push('=');
        out.push_str(&shell_quote(value));
        out.push_str("; ");
    }
    out.push_str(cmd);
    out
}

#[cfg(test)]
mod tests {
    use super::{account_env_prefix, build_agent_command, prefix_env, shell_quote};
    use crate::agent::{AgentConfig, AgentKind};

    /// The account wrapper is plain POSIX statements the interactive shell
    /// parses itself, so rc aliases (`cc-hub-new`) still resolve: no `/bin/sh
    /// -c` re-parse, no `env` binary. `export` scopes the whole command, not
    /// just its first simple command.
    #[test]
    fn account_env_prefix_is_posix_statements_before_the_command() {
        let unset = ["A".to_string(), "B".to_string()];
        let set = [("K".to_string(), "v w".to_string())];
        let cmd = "cc-hub-new --resume 'x' && echo done";
        let out = account_env_prefix(&unset, &set, cmd);
        assert_eq!(out, format!("unset A B; export K='v w'; {cmd}"));
        assert!(out.ends_with(&format!("; {cmd}")), "got: {out}");
        assert!(!out.contains("/bin/sh"), "got: {out}");
        assert!(!out.contains("env "), "got: {out}");
        assert_eq!(
            account_env_prefix(&unset, &[], "cc-hub-new"),
            "unset A B; cc-hub-new"
        );
        assert_eq!(
            account_env_prefix(&[], &set, "cc-hub-new"),
            "export K='v w'; cc-hub-new"
        );
        assert_eq!(account_env_prefix(&[], &[], "cc-hub-new"), "cc-hub-new");
    }

    #[test]
    fn claude_command_carries_model_flag() {
        let agent = AgentConfig {
            id: "claude".into(),
            kind: AgentKind::Claude,
            command: "claude".into(),
            use_bridge: false,
            models: Vec::new(),
        };
        let cmd = build_agent_command(
            &agent,
            "cchub-1-2",
            None,
            None,
            Some("claude-sonnet-5"),
            false,
        )
        .unwrap();
        assert!(
            cmd.contains("claude --model 'claude-sonnet-5'"),
            "got: {}",
            cmd
        );
        let cmd = build_agent_command(&agent, "cchub-1-2", None, None, None, false).unwrap();
        assert!(!cmd.contains("--model"), "got: {}", cmd);
    }

    #[test]
    fn pi_command_carries_model_flag() {
        let agent = AgentConfig {
            id: "pi-codex".into(),
            kind: AgentKind::Pi,
            command: "pi --provider openai-codex".into(),
            use_bridge: false,
            models: Vec::new(),
        };
        let cmd =
            build_agent_command(&agent, "cchub-1-2", None, None, Some("gpt-5.6"), false).unwrap();
        assert!(cmd.contains("--model 'gpt-5.6'"), "got: {}", cmd);
    }

    #[test]
    fn codex_new_session_carries_model_before_prompt() {
        let agent = AgentConfig {
            id: "codex".into(),
            kind: AgentKind::Codex,
            // Fixed flags (reasoning effort) come from config's command string.
            command: "codex -c model_reasoning_effort=high".into(),
            use_bridge: false,
            models: Vec::new(),
        };
        let cmd = build_agent_command(
            &agent,
            "cchub-1-2",
            None,
            Some("do the thing"),
            Some("gpt-5.6-luna"),
            false,
        )
        .unwrap();
        assert_eq!(
            cmd,
            "codex -c model_reasoning_effort=high -m 'gpt-5.6-luna' 'do the thing'"
        );
    }

    #[test]
    fn claude_resume_carries_the_positional_prompt() {
        let agent = AgentConfig {
            id: "claude".into(),
            kind: AgentKind::Claude,
            command: "claude".into(),
            use_bridge: false,
            models: Vec::new(),
        };
        let cmd = build_agent_command(
            &agent,
            "cchub-1-2",
            Some(super::SessionTarget::Resume("sid-1".into())),
            Some("pick up where you left off"),
            None,
            false,
        )
        .unwrap();
        assert!(
            cmd.ends_with("claude --resume 'sid-1' 'pick up where you left off'"),
            "got: {cmd}"
        );
    }

    #[test]
    fn claude_fresh_target_pins_the_session_id() {
        let agent = AgentConfig {
            id: "claude".into(),
            kind: AgentKind::Claude,
            command: "claude".into(),
            use_bridge: false,
            models: Vec::new(),
        };
        let cmd = build_agent_command(
            &agent,
            "cchub-1-2",
            Some(super::SessionTarget::Fresh("0000-fresh-uuid".into())),
            None,
            None,
            false,
        )
        .unwrap();
        assert!(
            cmd.ends_with("claude --session-id '0000-fresh-uuid'"),
            "got: {cmd}"
        );
    }

    #[test]
    fn codex_fresh_target_is_refused() {
        let agent = AgentConfig {
            id: "codex".into(),
            kind: AgentKind::Codex,
            command: "codex".into(),
            use_bridge: false,
            models: Vec::new(),
        };
        let err = build_agent_command(
            &agent,
            "cchub-1-2",
            Some(super::SessionTarget::Fresh("0000-fresh-uuid".into())),
            None,
            None,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("chosen session id"), "got: {err}");
    }

    #[test]
    fn codex_resume_uses_session_id_subcommand_not_model() {
        let agent = AgentConfig {
            id: "codex".into(),
            kind: AgentKind::Codex,
            command: "codex".into(),
            use_bridge: false,
            models: Vec::new(),
        };
        // A resumed session restores its own model, so `-m` must NOT be added.
        let cmd = build_agent_command(
            &agent,
            "cchub-1-2",
            Some(super::SessionTarget::Resume("019f60ca-uuid".into())),
            None,
            Some("gpt-5.6-luna"),
            false,
        )
        .unwrap();
        assert_eq!(cmd, "codex resume '019f60ca-uuid'");
    }

    #[test]
    fn codex_cannot_resume_by_file() {
        let agent = AgentConfig {
            id: "codex".into(),
            kind: AgentKind::Codex,
            command: "codex".into(),
            use_bridge: false,
            models: Vec::new(),
        };
        let err = build_agent_command(
            &agent,
            "cchub-1-2",
            Some(super::SessionTarget::ResumeFile("/x/s.jsonl".into())),
            None,
            None,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("resumes by session id"));
    }

    #[test]
    fn prefix_env_unix_is_posix_and_byte_identical() {
        // Unix keeps the POSIX `VAR='value' cmd` form.
        assert_eq!(
            prefix_env(
                "CLAUDE_CONFIG_DIR",
                "/home/u/.claude",
                "claude --resume x",
                false
            ),
            format!(
                "CLAUDE_CONFIG_DIR={} claude --resume x",
                shell_quote("/home/u/.claude")
            ),
        );
        // A value with a single quote gets POSIX close/escape/reopen quoting.
        assert_eq!(
            prefix_env("V", "a'b c", "cmd", false),
            format!("V={} cmd", shell_quote("a'b c")),
        );
    }

    #[test]
    fn prefix_env_windows_is_pwsh() {
        assert_eq!(
            prefix_env(
                "CLAUDE_CONFIG_DIR",
                r"C:\Users\u\.claude",
                "claude --resume x",
                true
            ),
            r"$env:CLAUDE_CONFIG_DIR = 'C:\Users\u\.claude'; claude --resume x",
        );
        // pwsh single-quote literal: an embedded single quote is doubled.
        assert_eq!(prefix_env("V", "a'b", "cmd", true), "$env:V = 'a''b'; cmd");
    }
}
