use log::{error, info};
use std::io;
use std::process::{Command, Stdio};

/// Spawn a new Claude session for `cwd`.
///
/// If cc-hub is running inside tmux, we open a sibling tmux window so the user
/// can switch to it with the usual tmux bindings.
///
/// Otherwise we launch a new terminal emulator window running `ccyo`. `pid_hint`
/// is the pid of the currently-selected session; when provided, we try to place
/// the new window on the same Hyprland workspace as that session.
pub fn spawn_claude_session(cwd: &str, pid_hint: Option<u32>) -> io::Result<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());

    if std::env::var("TMUX").is_ok() {
        spawn_tmux_window(cwd, &shell)
    } else {
        let workspace = pid_hint.and_then(crate::focus::workspace_for_pid);
        spawn_terminal(cwd, &shell, workspace)
    }
}

fn spawn_tmux_window(cwd: &str, shell: &str) -> io::Result<String> {
    let shell_cmd = format!("{} -ic ccyo", shell);
    let args = [
        "new-window",
        "-c",
        cwd,
        "-n",
        "claude",
        "-P",
        "-F",
        "#{session_name}:#{window_index}",
        &shell_cmd,
    ];
    info!("spawn: tmux {}", args.join(" "));

    let output = Command::new("tmux").args(args).output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    info!(
        "spawn: new-window status={} stdout={:?} stderr={:?}",
        output.status, stdout, stderr
    );
    if !output.status.success() {
        error!("spawn: new-window failed: {}", stderr);
        return Err(io::Error::other(format!(
            "tmux new-window failed: {}",
            if stderr.is_empty() { output.status.to_string() } else { stderr }
        )));
    }
    Ok(format!("spawned ccyo in tmux window {}", stdout))
}

fn spawn_terminal(cwd: &str, shell: &str, workspace: Option<i64>) -> io::Result<String> {
    let term = pick_terminal().ok_or_else(|| {
        io::Error::other("no terminal emulator found (set $TERMINAL or install kitty/foot/alacritty)")
    })?;

    let argv = term_argv(&term, cwd, shell);
    // Resolve the actual binary to run — some users wrap their terminal via a
    // dotfiles script (e.g. ~/.config/hypr/scripts/alacritty) that applies a
    // custom --config-file. Prefer that wrapper when it exists so the new
    // window matches the user's SUPER+Enter experience.
    let bin = resolve_terminal_binary(&term);

    // If we know the target Hyprland workspace, route the spawn through
    // `hyprctl dispatch exec` with a `[workspace N silent]` rule so Hyprland
    // places the new window there without switching focus away from cc-hub.
    if let Some(ws) = workspace {
        let mut parts: Vec<String> = Vec::with_capacity(argv.len() + 1);
        parts.push(shell_quote(&bin));
        for a in &argv {
            parts.push(shell_quote(a));
        }
        let exec_str = format!("[workspace {} silent] {}", ws, parts.join(" "));
        info!("spawn: hyprctl dispatch exec {}", exec_str);

        let output = Command::new("hyprctl")
            .args(["dispatch", "exec", &exec_str])
            .output();
        match output {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                info!(
                    "spawn: hyprctl exec ok, stdout={:?}",
                    stdout.trim()
                );
                return Ok(format!("spawned ccyo in {} on workspace {}", term, ws));
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                error!(
                    "spawn: hyprctl exec failed status={} stderr={:?}, falling back",
                    out.status,
                    stderr.trim()
                );
            }
            Err(e) => {
                error!("spawn: hyprctl not available ({}), falling back", e);
            }
        }
    }

    // Fallback: direct spawn (no workspace placement).
    info!("spawn: {} {}", bin, argv.join(" "));
    let child = Command::new(&bin)
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    match child {
        Ok(c) => {
            info!("spawn: {} pid={}", term, c.id());
            Ok(format!("spawned ccyo in {} window", term))
        }
        Err(e) => {
            error!("spawn: {} failed: {}", term, e);
            Err(e)
        }
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn resolve_terminal_binary(name: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        let candidate = format!("{}/.config/hypr/scripts/{}", home, name);
        if std::path::Path::new(&candidate).is_file() {
            return candidate;
        }
    }
    name.to_string()
}

fn pick_terminal() -> Option<String> {
    if let Ok(t) = std::env::var("TERMINAL") {
        if !t.is_empty() && has_binary(&t) {
            return Some(t);
        }
    }
    for t in ["kitty", "foot", "alacritty", "wezterm", "ghostty"] {
        if has_binary(t) {
            return Some(t.to_string());
        }
    }
    None
}

fn has_binary(name: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {} >/dev/null 2>&1", name)])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn term_argv(term: &str, cwd: &str, shell: &str) -> Vec<String> {
    let s = shell.to_string();
    match term {
        "alacritty" => vec![
            "--working-directory".into(), cwd.into(),
            "-e".into(), s, "-ic".into(), "ccyo".into(),
        ],
        "kitty" => vec![
            "--directory".into(), cwd.into(),
            s, "-ic".into(), "ccyo".into(),
        ],
        "foot" => vec![
            format!("--working-directory={}", cwd),
            s, "-ic".into(), "ccyo".into(),
        ],
        "wezterm" => vec![
            "start".into(), "--cwd".into(), cwd.into(),
            "--".into(), s, "-ic".into(), "ccyo".into(),
        ],
        "ghostty" => vec![
            format!("--working-directory={}", cwd),
            "-e".into(), format!("{} -ic ccyo", s),
        ],
        _ => vec!["-e".into(), s, "-ic".into(), "ccyo".into()],
    }
}
