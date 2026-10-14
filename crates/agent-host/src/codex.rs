//! Codex over `codex app-server` (JSON-RPC 2.0, one JSON object per line).
//!
//! A turn initializes, starts or resumes the thread with the Reader's MCP
//! server and the document's working folder as a writable sandbox (Codex
//! approves nothing else: commands run sandboxed, without asking), starts
//! the turn, and follows its notifications to `turn/completed`.

use std::{
    io::Write as _,
    sync::mpsc::RecvTimeoutError,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use glyphst_ai::agent::AgentCancellationToken;
use serde_json::{Value, json};

use crate::{
    external::{ExternalEvent, ExternalTurn, instructions},
    process::{AgentProcess, command},
};

/// The environment variable Codex reads the MCP token from.
const TOKEN_VARIABLE: &str = "GLYPHST_MCP_TOKEN";
/// How long Codex has to answer a request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const INTERRUPT_GRACE: Duration = Duration::from_secs(5);

struct Rpc {
    process: AgentProcess,
    next_id: u64,
    /// Whether an agent message was written, so the next starts a paragraph.
    wrote_message: bool,
}

impl Rpc {
    fn send(&mut self, message: Value) -> Result<()> {
        let stdin = self
            .process
            .stdin
            .as_mut()
            .context("Codex's stdin is closed")?;
        writeln!(stdin, "{message}").context("could not write to Codex")?;
        stdin.flush().context("could not write to Codex")
    }

    /// Sends a request and waits for its answer, handling what arrives
    /// meanwhile.
    fn request(
        &mut self,
        method: &str,
        params: Value,
        cancel: &AgentCancellationToken,
        events: &mut dyn FnMut(ExternalEvent),
    ) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        loop {
            if cancel.is_canceled() {
                bail!("agent run canceled");
            }
            if Instant::now() > deadline {
                bail!("Codex did not answer {method}");
            }
            let Some(message) = self.receive()? else {
                continue;
            };
            if message["id"] == json!(id) && message.get("method").is_none() {
                if let Some(error) = message.get("error") {
                    bail!(
                        "Codex refused {method}: {}",
                        error["message"].as_str().unwrap_or("unknown error")
                    );
                }
                return Ok(message["result"].clone());
            }
            self.dispatch(&message, events)?;
        }
    }

    /// The next message, or `None` after a short wait.
    fn receive(&mut self) -> Result<Option<Value>> {
        match self.process.lines.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => Ok(serde_json::from_str(&line).ok()),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(self.process.exit_error()),
        }
    }

    /// Handles a notification or a server request; returns the finished
    /// turn's status when `turn/completed` arrives.
    fn dispatch(
        &mut self,
        message: &Value,
        events: &mut dyn FnMut(ExternalEvent),
    ) -> Result<Option<Value>> {
        let method = message["method"].as_str().unwrap_or_default();
        let params = &message["params"];
        if message.get("id").is_some() && !method.is_empty() {
            // Server requests: approvals that slip past "never" are
            // accepted, questions and elicitations declined.
            let result = match method {
                "item/commandExecution/requestApproval"
                | "item/fileChange/requestApproval"
                | "applyPatchApproval"
                | "execCommandApproval" => json!({"decision": "accept"}),
                "item/tool/requestUserInput" => json!({"answers": {}}),
                "mcpServer/elicitation/request" => json!({"action": "decline"}),
                _ => {
                    return self
                        .send(json!({
                            "jsonrpc": "2.0",
                            "id": message["id"],
                            "error": {"code": -32601, "message": format!("{method} is not supported")},
                        }))
                        .map(|()| None);
                }
            };
            self.send(json!({"jsonrpc": "2.0", "id": message["id"], "result": result}))?;
            return Ok(None);
        }
        match method {
            "item/agentMessage/delta" => {
                events(ExternalEvent::Text(text(&params["delta"])));
            }
            "item/reasoning/summaryTextDelta" => {
                events(ExternalEvent::Reasoning(text(&params["delta"])));
            }
            "item/started" if params["item"]["type"] == "agentMessage" => {
                // Separate messages are separate paragraphs.
                if std::mem::replace(&mut self.wrote_message, true) {
                    events(ExternalEvent::Text("\n\n".into()));
                }
            }
            "item/started" => {
                if let Some(title) = item_title(&params["item"]) {
                    events(ExternalEvent::ToolStarted {
                        id: text(&params["item"]["id"]),
                        title,
                    });
                }
            }
            "item/completed" => {
                let item = &params["item"];
                if item_title(item).is_some() {
                    let status = item["status"].as_str().unwrap_or_default();
                    events(ExternalEvent::ToolFinished {
                        id: text(&item["id"]),
                        failed: matches!(status, "failed" | "declined")
                            || item["exitCode"].as_i64().is_some_and(|code| code != 0),
                    });
                }
            }
            "turn/completed" => return Ok(Some(params["turn"].clone())),
            _ => {}
        }
        Ok(None)
    }
}

pub fn run_turn(
    turn: &ExternalTurn,
    cancel: &AgentCancellationToken,
    events: &mut dyn FnMut(ExternalEvent),
) -> Result<()> {
    let mut command = command(&turn.agent.program, &turn.scratch.dir);
    command
        .arg("app-server")
        .env(TOKEN_VARIABLE, &turn.mcp.token);
    let process = AgentProcess::spawn(command, "Codex")?;
    let mut rpc = Rpc {
        process,
        next_id: 0,
        wrote_message: false,
    };
    rpc.request(
        "initialize",
        json!({
            "clientInfo": {"name": "glyphst", "title": "Glyphst Reader", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"experimentalApi": true},
        }),
        cancel,
        events,
    )?;
    rpc.send(json!({"jsonrpc": "2.0", "method": "initialized"}))?;

    let cwd = turn.scratch.dir.display().to_string();
    let mut thread = json!({
        "cwd": cwd,
        "approvalPolicy": "never",
        "sandbox": "workspace-write",
        "developerInstructions": instructions(&turn.scratch),
        "config": {
            "mcp_servers.glyphst.url": turn.mcp.url,
            "mcp_servers.glyphst.bearer_token_env_var": TOKEN_VARIABLE,
        },
    });
    if let Some(model) = &turn.model {
        thread["model"] = json!(model);
    }
    // A thread that cannot be resumed (deleted, another machine) starts
    // afresh.
    let resumed = match &turn.resume {
        Some(id) => {
            let mut params = thread.clone();
            params["threadId"] = json!(id);
            rpc.request("thread/resume", params, cancel, events).ok()
        }
        None => None,
    };
    let started = match resumed {
        Some(started) => started,
        None => rpc.request("thread/start", thread, cancel, events)?,
    };
    let thread_id = started["thread"]["id"]
        .as_str()
        .context("Codex started no thread")?
        .to_string();
    events(ExternalEvent::Session(thread_id.clone()));

    let mut input = vec![json!({"type": "text", "text": turn.prompt})];
    input.extend(turn.images.iter().map(|image| {
        json!({
            "type": "image",
            "url": format!("data:{};base64,{}", image.media_type, BASE64.encode(image.bytes.as_ref())),
        })
    }));
    let started = rpc.request(
        "turn/start",
        json!({"threadId": thread_id, "input": input, "summary": "auto"}),
        cancel,
        events,
    )?;
    let turn_id = text(&started["turn"]["id"]);

    let mut interrupted: Option<Instant> = None;
    loop {
        if cancel.is_canceled() && interrupted.is_none() {
            rpc.next_id += 1;
            let id = rpc.next_id;
            let _ = rpc.send(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "turn/interrupt",
                "params": {"threadId": thread_id, "turnId": turn_id},
            }));
            interrupted = Some(Instant::now());
        }
        if interrupted.is_some_and(|at| at.elapsed() > INTERRUPT_GRACE) {
            rpc.process.stop();
            bail!("agent run canceled");
        }
        let message = match rpc.receive() {
            Ok(Some(message)) => message,
            Ok(None) => continue,
            Err(_) if interrupted.is_some() => bail!("agent run canceled"),
            Err(error) => return Err(error),
        };
        if let Some(finished) = rpc.dispatch(&message, events)? {
            rpc.process.finish();
            if interrupted.is_some() {
                bail!("agent run canceled");
            }
            return match finished["status"].as_str() {
                Some("completed") => Ok(()),
                status => Err(anyhow!(
                    "Codex: {}",
                    finished["error"]["message"]
                        .as_str()
                        .or(status)
                        .unwrap_or("the turn failed")
                )),
            };
        }
    }
}

/// A short line for a tool item; `None` for items that are not tools.
fn item_title(item: &Value) -> Option<String> {
    let field = |key: &str| item[key].as_str().unwrap_or_default().to_string();
    Some(match item["type"].as_str()? {
        "commandExecution" => format!(
            "Ran {}",
            field("command").chars().take(60).collect::<String>()
        ),
        "fileChange" => {
            let files = item["changes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|change| change["path"].as_str())
                .map(|path| {
                    std::path::Path::new(path)
                        .file_name()
                        .map_or(path.to_string(), |name| name.to_string_lossy().into_owned())
                })
                .collect::<Vec<_>>();
            format!("Edited {}", files.join(", "))
        }
        "mcpToolCall" => field("tool").replace('_', " "),
        "dynamicToolCall" => field("tool").replace('_', " "),
        "webSearch" => format!("Searched the web for {}", field("query")),
        _ => return None,
    })
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_items_get_titles() {
        assert_eq!(
            item_title(&json!({"type": "commandExecution", "command": "ls -la"})).as_deref(),
            Some("Ran ls -la")
        );
        assert_eq!(
            item_title(&json!({"type": "fileChange", "changes": [{"path": "/x/notes.md"}]}))
                .as_deref(),
            Some("Edited notes.md")
        );
        assert_eq!(
            item_title(&json!({"type": "mcpToolCall", "tool": "read_document"})).as_deref(),
            Some("read document")
        );
        assert_eq!(item_title(&json!({"type": "agentMessage"})), None);
    }
}
