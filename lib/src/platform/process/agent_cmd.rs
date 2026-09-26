//! Pure classifiers for agent command lines: is this `pi` or `codex`, is a
//! codex process an interactive session, and which model or session it was
//! launched with.

use super::command_line;

/// Pure detector for Pi-agent invocations from `comm` + `cmdline`. Both
/// inputs must be lowercased. Exact-basename match on the first cmdline
/// argument prevents `/usr/bin/pipewire` from substring-matching `/bin/pi`.
pub(super) fn matches_pi_command(name: &str, cmd: &str) -> bool {
    if name == "pi" || name == "pi.exe" {
        return true;
    }
    let exe_basename = cmd
        .split_whitespace()
        .next()
        .and_then(|first| std::path::Path::new(first).file_name())
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if exe_basename == "pi" || exe_basename == "pi.exe" {
        return true;
    }
    cmd.contains("pi-coding-agent") || cmd.contains("@mariozechner/pi-coding-agent")
}

/// Pure detector for `codex` CLI invocations from `comm` + `cmdline`. Both
/// inputs must be lowercased. Exact-basename match on the first cmdline
/// argument avoids substring false positives (e.g. a `codex-something` binary),
/// while still catching a Homebrew/npm shim path like `/opt/homebrew/bin/codex`.
/// Detect the `codex` CLI by the OS-reported executable name (comm on Linux,
/// the `proc_pidpath` basename on macOS, the image name on Windows) — NOT the
/// command line. A joined command line loses argv[0] boundaries, and several
/// ChatGPT-desktop-app binaries live under spaced paths (`Codex Framework
/// .framework`, `Codex Computer Use.app`) whose basename would mis-extract onto
/// "codex"; the OS name splits those correctly ("SkyComputerUseService", etc.).
pub(super) fn matches_codex_command(name: &str) -> bool {
    name == "codex" || name == "codex.exe"
}

/// Codex subcommands that are NOT an interactive TUI session — background
/// servers, one-shot utilities, auth, etc. A `codex` process running one of
/// these is not a session the hub should surface (e.g. the ChatGPT desktop
/// app's `codex … app-server`, or `codex mcp-server`).
const CODEX_NON_INTERACTIVE: &[&str] = &[
    "exec",
    "e",
    "review",
    "login",
    "logout",
    "mcp",
    "mcp-server",
    "app-server",
    "app",
    "remote-control",
    "completion",
    "update",
    "doctor",
    "sandbox",
    "debug",
    "apply",
    "a",
    "archive",
    "delete",
    "unarchive",
    "cloud",
    "exec-server",
    "features",
    "help",
];

/// Codex options that consume the following token as their value, so the value
/// isn't mistaken for the subcommand. (`resume`/`fork` are interactive
/// subcommands and are deliberately absent from [`CODEX_NON_INTERACTIVE`].)
const CODEX_VALUE_FLAGS: &[&str] = &[
    "-c",
    "--config",
    "-m",
    "--model",
    "-s",
    "--sandbox",
    "-a",
    "--ask-for-approval",
    "--remote",
    "--enable",
    "--disable",
];

/// Whether a `codex` command line launches an interactive session (bare
/// `codex`, or `codex resume/fork …`) rather than a non-interactive subcommand.
/// Skips leading option/value pairs to find the first positional token.
///
/// `cmd` is expected lowercased (as [`super::is_agent_process`] passes it).
pub(super) fn codex_is_interactive(cmd: &str) -> bool {
    // The ChatGPT desktop app bundles a "Codex Framework" (an Electron/Chromium
    // app) plus a background `app-server`. Its helper processes are Chromium
    // `--type=` / crashpad children whose spaced framework path
    // ("Codex Framework.framework") makes basename extraction misfire onto
    // "codex". None of them are CLI sessions.
    if cmd.contains("chatgpt.app")
        || cmd.contains("codex framework")
        || cmd.contains("--type=")
        || cmd.contains("crashpad")
    {
        return false;
    }
    let toks: Vec<&str> = cmd.split_whitespace().skip(1).collect();
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i];
        if t.starts_with('-') {
            if CODEX_VALUE_FLAGS.contains(&t) && !t.contains('=') {
                i += 2;
            } else {
                i += 1;
            }
        } else {
            return !CODEX_NON_INTERACTIVE.contains(&t);
        }
    }
    // No positional token → a bare interactive `codex`.
    true
}

/// The `-m` / `--model` argument of a live codex process, so a session with no
/// rollout on disk yet can still show which model it launched with.
pub fn codex_model_arg(pid: u32) -> Option<String> {
    codex_model_from_cmd(&command_line(pid))
}

/// Session UUID passed to `codex resume`, when this process was launched by
/// resuming an existing conversation.
pub fn codex_resume_session_arg(pid: u32) -> Option<String> {
    codex_resume_session_from_cmd(&command_line(pid))
}

fn codex_model_from_cmd(cmd: &str) -> Option<String> {
    let toks: Vec<&str> = cmd.split_whitespace().collect();
    for (i, t) in toks.iter().enumerate() {
        if let Some(v) = t.strip_prefix("--model=") {
            return Some(v.trim_matches(['\'', '"']).to_string());
        }
        if (*t == "-m" || *t == "--model") && i + 1 < toks.len() {
            return Some(toks[i + 1].trim_matches(['\'', '"']).to_string());
        }
    }
    None
}

fn codex_resume_session_from_cmd(cmd: &str) -> Option<String> {
    let toks: Vec<&str> = cmd.split_whitespace().collect();
    let resume = toks.iter().position(|t| *t == "resume")?;
    toks.get(resume + 1)
        .filter(|sid| !sid.starts_with('-'))
        .map(|sid| sid.trim_matches(['\'', '"']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_detector_matches_real_pi_invocations() {
        assert!(matches_pi_command("pi", ""));
        assert!(matches_pi_command("pi.exe", ""));
        assert!(matches_pi_command(
            "node",
            "/usr/local/bin/pi --provider openai-codex --model gpt-5.4"
        ));
        assert!(matches_pi_command(
            "node",
            "node /home/u/.npm/_npx/abc/node_modules/pi-coding-agent/dist/cli.js"
        ));
        assert!(matches_pi_command(
            "node",
            "node /home/u/.npm/_npx/abc/node_modules/@mariozechner/pi-coding-agent/cli.js"
        ));
    }

    #[test]
    fn pi_detector_rejects_pipewire() {
        // Regression: `/usr/bin/pipewire` once substring-matched `/bin/pi`.
        assert!(!matches_pi_command("pipewire", "/usr/bin/pipewire"));
        assert!(!matches_pi_command(
            "pipewire-pulse",
            "/usr/bin/pipewire-pulse"
        ));
        assert!(!matches_pi_command("ping", "/usr/bin/ping 8.8.8.8"));
    }

    #[test]
    fn codex_detector_matches_exe_name_only() {
        // Matches on the OS-reported executable name, not the command line.
        assert!(matches_codex_command("codex"));
        assert!(matches_codex_command("codex.exe"));
        // Spaced-path desktop-app binaries resolve to their real basenames and
        // must NOT match (the bug the cmdline heuristic caused).
        assert!(!matches_codex_command("skycomputeruseservice"));
        assert!(!matches_codex_command("codex (service)"));
        assert!(!matches_codex_command("browser_crashpad_handler"));
        assert!(!matches_codex_command("codexer"));
    }

    #[test]
    fn codex_interactive_accepts_bare_and_resume() {
        // Bare interactive launch with only option/value pairs.
        assert!(codex_is_interactive(
            "/opt/homebrew/bin/codex -c model_reasoning_effort=high -m gpt-5.6-luna"
        ));
        // resume / fork produce interactive sessions.
        assert!(codex_is_interactive("codex resume 019f-uuid"));
        assert!(codex_is_interactive("codex fork --last"));
        // A trailing positional prompt is still interactive.
        assert!(codex_is_interactive("codex -m gpt-5.6-luna hello world"));
    }

    #[test]
    fn codex_interactive_rejects_servers_and_oneshots() {
        // The ChatGPT desktop app's background server.
        assert!(!codex_is_interactive(
            "/Applications/ChatGPT.app/Contents/Resources/codex -c features.code_mode_host=true app-server --analytics-default-enabled"
        ));
        assert!(!codex_is_interactive("codex mcp-server"));
        assert!(!codex_is_interactive("codex exec 'do a thing'"));
        // `-s sandbox` is a value, not the `sandbox` subcommand → still a
        // bare interactive launch.
        assert!(codex_is_interactive("codex -s sandbox -m gpt-5.6-luna"));
        // `codex sandbox <cmd>` as a subcommand is non-interactive.
        assert!(!codex_is_interactive("codex sandbox ls"));
    }

    #[test]
    fn codex_interactive_rejects_chatgpt_desktop_app_helpers() {
        // Inputs are lowercased, as is_agent_process passes them. A spaced
        // framework path would otherwise mis-extract basename "codex".
        assert!(!codex_is_interactive(
            "/applications/chatgpt.app/contents/frameworks/codex framework.framework/versions/150/helpers/browser_crashpad_handler --monitor-self"
        ));
        assert!(!codex_is_interactive(
            "/applications/chatgpt.app/contents/frameworks/codex framework.framework/versions/150/helpers/codex (service).app/contents/macos/codex (service) --type=gpu-process"
        ));
    }

    #[test]
    fn codex_model_arg_parsing() {
        assert_eq!(
            codex_model_from_cmd("/x/codex -c a=b -m gpt-5.6-luna").as_deref(),
            Some("gpt-5.6-luna")
        );
        assert_eq!(
            codex_model_from_cmd("codex --model='o3'").as_deref(),
            Some("o3")
        );
        assert_eq!(codex_model_from_cmd("codex resume 019f-uuid"), None);
    }

    #[test]
    fn codex_resume_arg_parsing() {
        assert_eq!(
            codex_resume_session_from_cmd(
                "/opt/homebrew/bin/codex -c model_reasoning_effort=high resume 019f94b1-ab14"
            )
            .as_deref(),
            Some("019f94b1-ab14")
        );
        assert_eq!(
            codex_resume_session_from_cmd("codex --yolo resume --last"),
            None
        );
        assert_eq!(codex_resume_session_from_cmd("codex --yolo"), None);
    }
}
