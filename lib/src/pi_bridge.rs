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

const BRIDGE_SOURCE: &str = r#"import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { mkdir, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";

type State = "idle" | "processing";

function latestModelFromEntries(entries: any[]): string | undefined {
	for (let i = entries.length - 1; i >= 0; i--) {
		const entry = entries[i];
		if (entry?.type === "model_change") {
			const provider = typeof entry.provider === "string" ? entry.provider : undefined;
			const modelId = typeof entry.modelId === "string" ? entry.modelId : undefined;
			if (provider && modelId) return `${provider}/${modelId}`;
		}
		if (entry?.type === "message" && entry.message?.role === "assistant") {
			const provider = typeof entry.message.provider === "string" ? entry.message.provider : undefined;
			const model = typeof entry.message.model === "string" ? entry.message.model : undefined;
			if (provider && model) return `${provider}/${model}`;
			if (model) return model;
		}
	}
	return undefined;
}

export default function (pi: ExtensionAPI) {
	const heartbeatDir = process.env.CC_HUB_HEARTBEAT_DIR;
	const tmux = process.env.CC_HUB_TMUX;
	const agent = process.env.CC_HUB_AGENT_ID ?? "pi";
	if (!heartbeatDir || !tmux) return;

	const heartbeatPath = join(heartbeatDir, `${tmux}.json`);
	let state: State = "idle";
	let model = process.env.CC_HUB_MODEL;

	const writeHeartbeat = async (ctx: any) => {
		const sessionFile = ctx.sessionManager.getSessionFile();
		const sessionId = ctx.sessionManager.getSessionId();
		if (!model) model = latestModelFromEntries(ctx.sessionManager.getEntries());
		const payload = {
			agent,
			pid: process.pid,
			tmux,
			cwd: ctx.cwd,
			sessionFile,
			sessionId,
			state,
			model,
			updatedAt: Date.now(),
		};
		await mkdir(dirname(heartbeatPath), { recursive: true });
		await writeFile(heartbeatPath, JSON.stringify(payload), "utf8");
	};

	pi.on("session_start", async (_event, ctx) => {
		state = "idle";
		await writeHeartbeat(ctx);
	});

	pi.on("model_select", async (event, ctx) => {
		model = `${event.model.provider}/${event.model.id}`;
		await writeHeartbeat(ctx);
	});

	pi.on("agent_start", async (_event, ctx) => {
		state = "processing";
		await writeHeartbeat(ctx);
	});

	pi.on("tool_execution_start", async (_event, ctx) => {
		state = "processing";
		await writeHeartbeat(ctx);
	});

	pi.on("agent_end", async (_event, ctx) => {
		state = "idle";
		await writeHeartbeat(ctx);
	});

	pi.on("session_shutdown", async (_event, ctx) => {
		state = "idle";
		await writeHeartbeat(ctx);
	});
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
