//! Process-control boundary used by application controllers.
//!
//! Keeping tmux and agent spawning behind this trait lets Sessions/Tasks logic
//! be exercised without shelling out and prevents UI state methods from being
//! permanently coupled to the platform implementation.

use crate::spawn::ResumeTarget;
use std::io;

pub trait AgentRuntime: Send + Sync {
    fn session_exists(&self, tmux: &str) -> bool;
    fn ready_for_input(&self, tmux: &str) -> bool;
    fn send_prompt(&self, tmux: &str, prompt: &str) -> io::Result<()>;
    fn kill_session(&self, tmux: &str) -> io::Result<()>;
    fn spawn_session(
        &self,
        agent_id: &str,
        cwd: &str,
        resume: Option<ResumeTarget>,
        initial_prompt: Option<&str>,
        readonly_tools: bool,
    ) -> io::Result<String>;
}

#[derive(Default)]
pub struct SystemAgentRuntime;

impl AgentRuntime for SystemAgentRuntime {
    fn session_exists(&self, tmux: &str) -> bool {
        crate::send::tmux_session_exists(tmux)
    }

    fn ready_for_input(&self, tmux: &str) -> bool {
        crate::send::pane_ready_for_input(tmux)
    }

    fn send_prompt(&self, tmux: &str, prompt: &str) -> io::Result<()> {
        crate::send::send_prompt(tmux, prompt)
    }

    fn kill_session(&self, tmux: &str) -> io::Result<()> {
        crate::send::kill_tmux_session(tmux)
    }

    fn spawn_session(
        &self,
        agent_id: &str,
        cwd: &str,
        resume: Option<ResumeTarget>,
        initial_prompt: Option<&str>,
        readonly_tools: bool,
    ) -> io::Result<String> {
        crate::spawn::spawn_agent_session(agent_id, cwd, resume, initial_prompt, readonly_tools)
    }
}
