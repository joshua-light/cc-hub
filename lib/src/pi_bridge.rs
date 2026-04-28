use crate::platform::paths;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HeartbeatState {
    Idle,
    Processing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Heartbeat {
    pub agent: String,
    pub pid: u32,
    pub tmux: String,
    pub cwd: String,
    pub session_file: Option<PathBuf>,
    pub session_id: Option<String>,
    pub state: HeartbeatState,
    pub model: Option<String>,
    pub updated_at: u64,
}

const BRIDGE_SOURCE: &str = r#"import type { ExtensionAPI } from \"@mariozechner/pi-coding-agent\";
import { mkdir, writeFile } from \"node:fs/promises\";
import { dirname, join } from \"node:path\";

type State = \"idle\" | \"processing\";

function latestModelFromEntries(entries: any[]): string | undefined {
\tfor (let i = entries.length - 1; i >= 0; i--) {
\t\tconst entry = entries[i];
\t\tif (entry?.type === \"model_change\") {
\t\t\tconst provider = typeof entry.provider === \"string\" ? entry.provider : undefined;
\t\t\tconst modelId = typeof entry.modelId === \"string\" ? entry.modelId : undefined;
\t\t\tif (provider && modelId) return `${provider}/${modelId}`;
\t\t}
\t\tif (entry?.type === \"message\" && entry.message?.role === \"assistant\") {
\t\t\tconst provider = typeof entry.message.provider === \"string\" ? entry.message.provider : undefined;
\t\t\tconst model = typeof entry.message.model === \"string\" ? entry.message.model : undefined;
\t\t\tif (provider && model) return `${provider}/${model}`;
\t\t\tif (model) return model;
\t\t}
\t}
\treturn undefined;
}

export default function (pi: ExtensionAPI) {
\tconst heartbeatDir = process.env.CC_HUB_HEARTBEAT_DIR;
\tconst tmux = process.env.CC_HUB_TMUX;
\tconst agent = process.env.CC_HUB_AGENT_ID ?? \"pi\";
\tif (!heartbeatDir || !tmux) return;

\tconst heartbeatPath = join(heartbeatDir, `${tmux}.json`);
\tlet state: State = \"idle\";
\tlet model = process.env.CC_HUB_MODEL;

\tconst writeHeartbeat = async (ctx: any) => {
\t\tconst sessionFile = ctx.sessionManager.getSessionFile();
\t\tconst sessionId = ctx.sessionManager.getSessionId();
\t\tif (!model) model = latestModelFromEntries(ctx.sessionManager.getEntries());
\t\tconst payload = {
\t\t\tagent,
\t\t\tpid: process.pid,
\t\t\ttmux,
\t\t\tcwd: ctx.cwd,
\t\t\tsessionFile,
\t\t\tsessionId,
\t\t\tstate,
\t\t\tmodel,
\t\t\tupdatedAt: Date.now(),
\t\t};
\t\tawait mkdir(dirname(heartbeatPath), { recursive: true });
\t\tawait writeFile(heartbeatPath, JSON.stringify(payload), \"utf8\");
\t};

\tpi.on(\"session_start\", async (_event, ctx) => {
\t\tstate = \"idle\";
\t\tawait writeHeartbeat(ctx);
\t});

\tpi.on(\"model_select\", async (event, ctx) => {
\t\tmodel = `${event.model.provider}/${event.model.id}`;
\t\tawait writeHeartbeat(ctx);
\t});

\tpi.on(\"agent_start\", async (_event, ctx) => {
\t\tstate = \"processing\";
\t\tawait writeHeartbeat(ctx);
\t});

\tpi.on(\"tool_execution_start\", async (_event, ctx) => {
\t\tstate = \"processing\";
\t\tawait writeHeartbeat(ctx);
\t});

\tpi.on(\"agent_end\", async (_event, ctx) => {
\t\tstate = \"idle\";
\t\tawait writeHeartbeat(ctx);
\t});

\tpi.on(\"session_shutdown\", async (_event, ctx) => {
\t\tstate = \"idle\";
\t\tawait writeHeartbeat(ctx);
\t});
}
"#;

pub fn ensure_bridge_file() -> io::Result<PathBuf> {
    let path = paths::pi_bridge_file()
        .ok_or_else(|| io::Error::other("home dir unavailable for pi bridge"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let needs_write = match std::fs::read_to_string(&path) {
        Ok(existing) => existing != BRIDGE_SOURCE,
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => return Err(e),
    };
    if needs_write {
        std::fs::write(&path, BRIDGE_SOURCE)?;
    }
    Ok(path)
}

pub fn load_heartbeats() -> Vec<Heartbeat> {
    let Some(dir) = paths::pi_heartbeats_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .filter_map(|raw| serde_json::from_str::<Heartbeat>(&raw).ok())
        .collect()
}
