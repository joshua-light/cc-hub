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

/// Shared test double: records every runtime call so controller tests can
/// assert on the process-control traffic without a terminal or tmux.
#[cfg(test)]
pub(crate) mod testing {
    use super::AgentRuntime;
    use crate::spawn::ResumeTarget;
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    /// One recorded [`AgentRuntime::spawn_session`] call. `resume` is kept as
    /// a rendered string because [`ResumeTarget`] is what tests assert on
    /// textually; `initial_prompt` distinguishes inline-vs-queued dispatch.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct SpawnCall {
        pub agent_id: String,
        pub cwd: String,
        pub resume: Option<String>,
        pub initial_prompt: Option<String>,
        pub readonly_tools: bool,
    }

    pub struct RecordingRuntime {
        pub prompts: Mutex<Vec<(String, String)>>,
        pub spawns: Mutex<Vec<SpawnCall>>,
        pub kills: Mutex<Vec<String>>,
        /// Name returned by successful spawns.
        pub spawn_name: String,
        /// When set, spawn_session fails with this message.
        pub spawn_error: Option<String>,
        pub exists: AtomicBool,
        pub ready: AtomicBool,
    }

    impl Default for RecordingRuntime {
        fn default() -> Self {
            Self {
                prompts: Mutex::new(Vec::new()),
                spawns: Mutex::new(Vec::new()),
                kills: Mutex::new(Vec::new()),
                spawn_name: "mock-spawn".into(),
                spawn_error: None,
                exists: AtomicBool::new(true),
                ready: AtomicBool::new(true),
            }
        }
    }

    impl AgentRuntime for RecordingRuntime {
        fn session_exists(&self, _tmux: &str) -> bool {
            self.exists.load(Ordering::Relaxed)
        }
        fn ready_for_input(&self, _tmux: &str) -> bool {
            self.ready.load(Ordering::Relaxed)
        }
        fn send_prompt(&self, tmux: &str, prompt: &str) -> io::Result<()> {
            self.prompts
                .lock()
                .unwrap()
                .push((tmux.to_string(), prompt.to_string()));
            Ok(())
        }
        fn kill_session(&self, tmux: &str) -> io::Result<()> {
            self.kills.lock().unwrap().push(tmux.to_string());
            Ok(())
        }
        fn spawn_session(
            &self,
            agent_id: &str,
            cwd: &str,
            resume: Option<ResumeTarget>,
            initial_prompt: Option<&str>,
            readonly_tools: bool,
        ) -> io::Result<String> {
            self.spawns.lock().unwrap().push(SpawnCall {
                agent_id: agent_id.to_string(),
                cwd: cwd.to_string(),
                resume: resume.map(|r| format!("{:?}", r)),
                initial_prompt: initial_prompt.map(str::to_string),
                readonly_tools,
            });
            match &self.spawn_error {
                Some(msg) => Err(io::Error::other(msg.clone())),
                None => Ok(self.spawn_name.clone()),
            }
        }
    }
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
