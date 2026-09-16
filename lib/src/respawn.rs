//! Continue an existing session on another subscription account.
//!
//! The rules mirror the resource broker's worker replacement
//! (`resource_manager.py`): a Claude session moving between Claude homes is
//! resumed natively — its transcript is copied into the target account's
//! `projects/` so `--resume` finds it — and every other pairing starts a
//! fresh session that reads the old transcript. Native resume stays limited
//! to the paths the broker has proven; see docs/account-routing-design.md.

use crate::agent::AgentKind;
use crate::models::SessionInfo;
use crate::resources::Account;
use std::io;
use std::path::{Path, PathBuf};

/// How the respawned session picks up the old one's work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Continuation {
    /// `--resume <session id>` under the target account, mid-conversation.
    /// `carry` is the transcript copy that makes the id resolvable there;
    /// [`None`] when the transcript already lives in the target home.
    Resume {
        session_id: String,
        carry: Option<Carry>,
    },
    /// A fresh session whose opening prompt points at the old transcript —
    /// the only honest continuation across providers, and between Codex
    /// homes, where native cross-home resume is untested.
    Handoff { transcript: PathBuf },
}

/// A pending transcript copy into the target account's `projects/` dir.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Carry {
    pub from: PathBuf,
    pub to: PathBuf,
}

impl Carry {
    pub fn perform(&self) -> io::Result<()> {
        if let Some(parent) = self.to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&self.from, &self.to).map(|_| ())
    }
}

impl Continuation {
    /// Decide how `session` continues on the account `target_id`. Pure — the
    /// transcript copy is planned, not performed; the caller runs
    /// [`Carry::perform`] right before spawning.
    pub fn plan(session: &SessionInfo, target_id: &str, target: &Account) -> io::Result<Self> {
        let transcript = || {
            session
                .jsonl_path
                .clone()
                .ok_or_else(|| io::Error::other("session has no transcript on disk"))
        };
        match (session.agent_kind, target.provider) {
            (AgentKind::Claude, AgentKind::Claude) => {
                let from = transcript()?;
                let home = target
                    .home()
                    .ok_or_else(|| io::Error::other("target account home unavailable"))?;
                let to = home
                    .join("projects")
                    .join(crate::scanner::encode_path(&session.cwd))
                    .join(format!("{}.jsonl", session.session_id));
                let carry = (from != to).then_some(Carry { from, to });
                Ok(Continuation::Resume {
                    session_id: session.session_id.clone(),
                    carry,
                })
            }
            // Same Codex home: the rollout is already where `codex resume`
            // looks, exactly the Sessions-grid resume path.
            (AgentKind::Codex, AgentKind::Codex) if session.agent_id == target_id => {
                Ok(Continuation::Resume {
                    session_id: session.session_id.clone(),
                    carry: None,
                })
            }
            _ => Ok(Continuation::Handoff {
                transcript: transcript()?,
            }),
        }
    }

    /// One-word row label for the account picker.
    pub fn label(&self) -> &'static str {
        match self {
            Continuation::Resume { .. } => "resume",
            Continuation::Handoff { .. } => "handoff",
        }
    }
}

/// Opening prompt of a natively resumed session (the broker's `RESUMED`).
pub const RESUMED: &str = "This session continues an earlier one that stopped mid-work — likely \
its account ran dry. Pick up exactly where the transcript stops; do not repeat an external \
write (push, build, PR) without checking whether it landed.";

/// Opening prompt of a handoff session: same contract, but the context has
/// to be recovered from the transcript first.
pub fn handoff_prompt(transcript: &Path) -> String {
    format!(
        "You are taking over an earlier session that stopped mid-work — likely its account ran \
dry. Its transcript is at {}. Read it to recover the context, then pick up exactly where it \
stops; do not repeat an external write (push, build, PR) without checking whether it landed.",
        transcript.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(provider: AgentKind, home: Option<&str>) -> Account {
        Account {
            provider,
            home: home.map(str::to_string),
            home_mode: None,
            executable: None,
            args: Vec::new(),
        }
    }

    fn session(kind: AgentKind, agent_id: &str, jsonl: Option<&str>) -> SessionInfo {
        SessionInfo {
            agent_id: agent_id.into(),
            agent_kind: kind,
            pid: 1,
            session_id: "sid-1".into(),
            cwd: "/tmp/proj".into(),
            project_name: "proj".into(),
            started_at: 0,
            last_activity: None,
            state: crate::models::SessionState::Inactive,
            last_user_message: None,
            summary: None,
            title: None,
            titling: false,
            model: None,
            git_branch: None,
            version: None,
            jsonl_path: jsonl.map(PathBuf::from),
            tmux_session: None,
            current_tool: None,
            is_thinking: false,
            context_tokens: None,
            tool_uses_count: 0,
        }
    }

    #[test]
    fn claude_to_claude_carries_the_transcript_into_the_target_home() {
        let source = session(
            AgentKind::Claude,
            "cc-1",
            Some("/h/.claude/projects/x/sid-1.jsonl"),
        );
        let target = account(AgentKind::Claude, Some("/h/.claude-personal"));
        let plan = Continuation::plan(&source, "cc-2", &target).unwrap();
        assert_eq!(
            plan,
            Continuation::Resume {
                session_id: "sid-1".into(),
                carry: Some(Carry {
                    from: "/h/.claude/projects/x/sid-1.jsonl".into(),
                    // `/tmp/proj` encodes the way Claude names project dirs.
                    to: "/h/.claude-personal/projects/-tmp-proj/sid-1.jsonl".into(),
                }),
            }
        );
    }

    #[test]
    fn claude_transcript_already_home_needs_no_carry() {
        let source = session(
            AgentKind::Claude,
            "cc-1",
            Some("/h/.claude/projects/-tmp-proj/sid-1.jsonl"),
        );
        let target = account(AgentKind::Claude, Some("/h/.claude"));
        let plan = Continuation::plan(&source, "cc-1", &target).unwrap();
        assert_eq!(
            plan,
            Continuation::Resume {
                session_id: "sid-1".into(),
                carry: None,
            }
        );
    }

    #[test]
    fn claude_without_transcript_cannot_move_accounts() {
        let source = session(AgentKind::Claude, "cc-1", None);
        let target = account(AgentKind::Claude, Some("/h/.claude-personal"));
        let err = Continuation::plan(&source, "cc-2", &target).unwrap_err();
        assert!(err.to_string().contains("no transcript"), "got: {err}");
    }

    #[test]
    fn codex_resumes_only_its_own_home_and_hands_off_to_the_other() {
        let source = session(
            AgentKind::Codex,
            "codex-1",
            Some("/h/.codex/sessions/r.jsonl"),
        );
        let same = account(AgentKind::Codex, Some("~/.codex"));
        assert_eq!(
            Continuation::plan(&source, "codex-1", &same).unwrap(),
            Continuation::Resume {
                session_id: "sid-1".into(),
                carry: None,
            }
        );
        let other = account(AgentKind::Codex, Some("~/.codex-personal"));
        assert_eq!(
            Continuation::plan(&source, "codex-2", &other).unwrap(),
            Continuation::Handoff {
                transcript: "/h/.codex/sessions/r.jsonl".into(),
            }
        );
    }

    #[test]
    fn cross_provider_is_always_a_handoff() {
        let source = session(
            AgentKind::Claude,
            "cc-1",
            Some("/h/.claude/projects/x/sid-1.jsonl"),
        );
        let target = account(AgentKind::Codex, Some("~/.codex"));
        assert_eq!(
            Continuation::plan(&source, "codex-1", &target).unwrap(),
            Continuation::Handoff {
                transcript: "/h/.claude/projects/x/sid-1.jsonl".into(),
            }
        );
    }
}
