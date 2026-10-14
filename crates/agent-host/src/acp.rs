//! Any agent that speaks the Agent Client Protocol (v1, JSON-RPC 2.0 over
//! stdio): `initialize`, then `session/load` or `session/new` with the
//! Reader's MCP server (over HTTP when the agent supports it, otherwise
//! through `glyphst-reader --mcp-stdio`), then `session/prompt`.
//!
//! Permission requests are answered with the agent's "allow once" option.
//! The Reader offers no file system or terminal of its own, so the agent
//! uses its own tools; their writes are not confined to the working folder.

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
    mcp_stdio::{TOKEN_VARIABLE, URL_VARIABLE},
    process::{AgentProcess, command},
};

const PROTOCOL_VERSION: u64 = 1;
/// How long the agent has to answer setup requests.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const INTERRUPT_GRACE: Duration = Duration::from_secs(5);

struct Acp {
    process: AgentProcess,
    next_id: u64,
    /// Session updates are ignored while a loaded session replays its
    /// history.
    replaying: bool,
}

impl Acp {
    fn send(&mut self, message: Value) -> Result<()> {
        let stdin = self
            .process
            .stdin
            .as_mut()
            .context("the agent's stdin is closed")?;
        writeln!(stdin, "{message}").context("could not write to the agent")?;
        stdin.flush().context("could not write to the agent")
    }

    fn start_request(&mut self, method: &str, params: Value) -> Result<u64> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        Ok(id)
    }

    /// Sends a request and waits for its answer, handling what arrives
    /// meanwhile; `timeout` is `None` for prompts, which take as long as
    /// they take.
    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
        cancel: &AgentCancellationToken,
        events: &mut dyn FnMut(ExternalEvent),
    ) -> Result<Value> {
        let id = self.start_request(method, params)?;
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        loop {
            if cancel.is_canceled() {
                bail!("agent run canceled");
            }
            if deadline.is_some_and(|deadline| Instant::now() > deadline) {
                bail!("the agent did not answer {method}");
            }
            let Some(message) = self.receive()? else {
                continue;
            };
            if message["id"] == json!(id) && message.get("method").is_none() {
                if let Some(error) = message.get("error") {
                    bail!(
                        "the agent refused {method}: {}",
                        error["message"].as_str().unwrap_or("unknown error")
                    );
                }
                return Ok(message["result"].clone());
            }
            self.dispatch(&message, events)?;
        }
    }

    fn receive(&mut self) -> Result<Option<Value>> {
        match self.process.lines.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => Ok(serde_json::from_str(&line).ok()),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(self.process.exit_error()),
        }
    }

    fn dispatch(&mut self, message: &Value, events: &mut dyn FnMut(ExternalEvent)) -> Result<()> {
        let method = message["method"].as_str().unwrap_or_default();
        let params = &message["params"];
        if message.get("id").is_some() && !method.is_empty() {
            let reply = match method {
                "session/request_permission" => {
                    json!({"result": {"outcome": {"outcome": "selected", "optionId": allow_option(&params["options"])}}})
                }
                _ => {
                    json!({"error": {"code": -32601, "message": format!("{method} is not supported")}})
                }
            };
            let mut reply = reply;
            reply["jsonrpc"] = json!("2.0");
            reply["id"] = message["id"].clone();
            return self.send(reply);
        }
        if method != "session/update" || self.replaying {
            return Ok(());
        }
        let update = &params["update"];
        let chunk = || {
            update["content"]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        match update["sessionUpdate"].as_str().unwrap_or_default() {
            "agent_message_chunk" => events(ExternalEvent::Text(chunk())),
            "agent_thought_chunk" => events(ExternalEvent::Reasoning(chunk())),
            "tool_call" => events(ExternalEvent::ToolStarted {
                id: text(&update["toolCallId"]),
                title: update["title"].as_str().unwrap_or("Tool").to_string(),
            }),
            "tool_call_update" => {
                if let Some(status @ ("completed" | "failed")) = update["status"].as_str() {
                    events(ExternalEvent::ToolFinished {
                        id: text(&update["toolCallId"]),
                        failed: status == "failed",
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// The option that allows a tool call once (or always, or the first).
fn allow_option(options: &Value) -> Value {
    let options = options.as_array().map(Vec::as_slice).unwrap_or_default();
    ["allow_once", "allow_always"]
        .iter()
        .find_map(|kind| options.iter().find(|option| option["kind"] == *kind))
        .or(options.first())
        .map(|option| option["optionId"].clone())
        .unwrap_or(Value::Null)
}

pub fn run_turn(
    turn: &ExternalTurn,
    cancel: &AgentCancellationToken,
    events: &mut dyn FnMut(ExternalEvent),
) -> Result<()> {
    let mut command = command(&turn.agent.program, &turn.scratch.dir);
    command.args(&turn.agent.args);
    command.envs(turn.agent.env.iter().map(|(key, value)| (key, value)));
    let process = AgentProcess::spawn(command, &turn.agent.name)?;
    let mut acp = Acp {
        process,
        next_id: 0,
        replaying: false,
    };
    let initialized = acp.request(
        "initialize",
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
            "clientInfo": {"name": "glyphst", "title": "Glyphst Reader", "version": env!("CARGO_PKG_VERSION")},
        }),
        Some(REQUEST_TIMEOUT),
        cancel,
        events,
    )?;
    let capabilities = &initialized["agentCapabilities"];
    let mcp = mcp_server(turn, capabilities["mcpCapabilities"]["http"] == true)?;
    let cwd = turn.scratch.dir.display().to_string();

    let mut session = None;
    if let Some(id) = &turn.resume
        && capabilities["loadSession"] == true
    {
        acp.replaying = true;
        let loaded = acp.request(
            "session/load",
            json!({"sessionId": id, "cwd": cwd, "mcpServers": [mcp]}),
            Some(REQUEST_TIMEOUT),
            cancel,
            events,
        );
        acp.replaying = false;
        if loaded.is_ok() {
            session = Some(id.clone());
        }
    }
    let session = match session {
        Some(session) => session,
        None => {
            let created = acp.request(
                "session/new",
                json!({"cwd": cwd, "mcpServers": [mcp]}),
                Some(REQUEST_TIMEOUT),
                cancel,
                events,
            )?;
            text(&created["sessionId"])
        }
    };
    if session.is_empty() {
        bail!("{} started no session", turn.agent.name);
    }
    events(ExternalEvent::Session(session.clone()));

    // ACP has no system prompt; the first message of a session carries the
    // instructions.
    let mut prompt = if turn.resume.is_none() {
        format!("{}\n\n{}", instructions(&turn.scratch), turn.prompt)
    } else {
        turn.prompt.clone()
    };
    if capabilities["promptCapabilities"]["image"] != true && !turn.images.is_empty() {
        prompt.push_str("\n\n(The user attached images this agent cannot read.)");
    }
    let mut blocks = vec![json!({"type": "text", "text": prompt})];
    if capabilities["promptCapabilities"]["image"] == true {
        blocks.extend(turn.images.iter().map(|image| {
            json!({"type": "image", "data": BASE64.encode(image.bytes.as_ref()), "mimeType": image.media_type})
        }));
    }
    let id = acp.start_request(
        "session/prompt",
        json!({"sessionId": session, "prompt": blocks}),
    )?;
    let mut canceled: Option<Instant> = None;
    loop {
        if cancel.is_canceled() && canceled.is_none() {
            let _ = acp.send(json!({
                "jsonrpc": "2.0",
                "method": "session/cancel",
                "params": {"sessionId": session},
            }));
            canceled = Some(Instant::now());
        }
        if canceled.is_some_and(|at| at.elapsed() > INTERRUPT_GRACE) {
            acp.process.stop();
            bail!("agent run canceled");
        }
        let message = match acp.receive() {
            Ok(Some(message)) => message,
            Ok(None) => continue,
            Err(_) if canceled.is_some() => bail!("agent run canceled"),
            Err(error) => return Err(error),
        };
        if message["id"] == json!(id) && message.get("method").is_none() {
            acp.process.finish();
            if canceled.is_some() {
                bail!("agent run canceled");
            }
            if let Some(error) = message.get("error") {
                return Err(anyhow!(
                    "{}: {}",
                    turn.agent.name,
                    error["message"].as_str().unwrap_or("the turn failed")
                ));
            }
            return match message["result"]["stopReason"].as_str() {
                Some("refusal") => Err(anyhow!("{} declined the request", turn.agent.name)),
                _ => Ok(()),
            };
        }
        acp.dispatch(&message, events)?;
    }
}

/// The Reader's MCP server as the agent can reach it.
fn mcp_server(turn: &ExternalTurn, http: bool) -> Result<Value> {
    if http {
        return Ok(json!({
            "type": "http",
            "name": "glyphst",
            "url": turn.mcp.url,
            "headers": [{"name": "Authorization", "value": format!("Bearer {}", turn.mcp.token)}],
        }));
    }
    let reader = std::env::current_exe().context("could not find the Reader's program")?;
    Ok(json!({
        "name": "glyphst",
        "command": reader,
        "args": ["--mcp-stdio"],
        "env": [
            {"name": URL_VARIABLE, "value": turn.mcp.url},
            {"name": TOKEN_VARIABLE, "value": turn.mcp.token},
        ],
    }))
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_requests_choose_allowing_once() {
        let options = json!([
            {"optionId": "a", "kind": "reject_once"},
            {"optionId": "b", "kind": "allow_always"},
            {"optionId": "c", "kind": "allow_once"},
        ]);
        assert_eq!(allow_option(&options), json!("c"));
        assert_eq!(
            allow_option(&json!([{"optionId": "x", "kind": "reject_once"}])),
            json!("x")
        );
    }
}
