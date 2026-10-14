//! Pi over `pi --mode rpc` (JSONL records, not JSON-RPC). A per-run
//! extension registers the Reader's MCP server and blocks file writes
//! outside the document's working folder; Pi has no other permission
//! prompts.

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

const EXTENSION: &str = include_str!("pi/glyphst.mjs");
const INTERRUPT_GRACE: Duration = Duration::from_secs(5);

pub fn run_turn(
    turn: &ExternalTurn,
    cancel: &AgentCancellationToken,
    events: &mut dyn FnMut(ExternalEvent),
) -> Result<()> {
    match run(turn, turn.resume.as_deref(), cancel, events) {
        // A session Pi cannot open (deleted, moved) starts afresh.
        Err(error) if turn.resume.is_some() && error.to_string().contains("session") => {
            run(turn, None, cancel, events)
        }
        result => result,
    }
}

fn run(
    turn: &ExternalTurn,
    resume: Option<&str>,
    cancel: &AgentCancellationToken,
    events: &mut dyn FnMut(ExternalEvent),
) -> Result<()> {
    let extension =
        std::env::temp_dir().join(format!("glyphst-pi-{}.mjs", uuid::Uuid::new_v4().simple()));
    std::fs::write(&extension, EXTENSION).context("could not write Pi's extension")?;
    let _cleanup = Cleanup(extension.clone());
    let mut command = command(&turn.agent.program, &turn.scratch.dir);
    command
        .args(["--mode", "rpc", "--extension"])
        .arg(&extension)
        .arg("--append-system-prompt")
        .arg(instructions(&turn.scratch))
        .env("GLYPHST_SCRATCH", &turn.scratch.dir)
        .env("GLYPHST_MCP_URL", &turn.mcp.url)
        .env("GLYPHST_MCP_TOKEN", &turn.mcp.token);
    if let Some(model) = &turn.model {
        command.args(["--model", model]);
    }
    if let Some(session) = resume {
        command.args(["--session", session]);
    }
    let mut process = AgentProcess::spawn(command, "Pi")?;
    let images = turn
        .images
        .iter()
        .map(|image| {
            json!({"type": "image", "data": BASE64.encode(image.bytes.as_ref()), "mimeType": image.media_type})
        })
        .collect::<Vec<_>>();
    send(
        &mut process,
        json!({"id": "prompt", "type": "prompt", "message": turn.prompt, "images": images}),
    )?;

    let mut aborted: Option<Instant> = None;
    let mut settled = false;
    loop {
        if cancel.is_canceled() && aborted.is_none() {
            let _ = send(&mut process, json!({"type": "abort"}));
            aborted = Some(Instant::now());
        }
        if aborted.is_some_and(|at| at.elapsed() > INTERRUPT_GRACE) {
            process.stop();
            bail!("agent run canceled");
        }
        let line = match process.lines.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                if aborted.is_some() {
                    bail!("agent run canceled");
                }
                return Err(process.exit_error());
            }
        };
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match record["type"].as_str().unwrap_or_default() {
            "response" if record["id"] == "prompt" => {
                if record["success"] == false {
                    let error = record["error"].as_str().unwrap_or("Pi refused the prompt");
                    return Err(anyhow!("Pi: {error}"));
                }
                if record["data"]["disposition"] == "handled" {
                    settled = true;
                    send(&mut process, json!({"id": "state", "type": "get_state"}))?;
                }
            }
            "response" if record["id"] == "state" => {
                if let Some(session) = record["data"]["sessionId"].as_str() {
                    events(ExternalEvent::Session(session.to_string()));
                }
                process.finish();
                if aborted.is_some() {
                    bail!("agent run canceled");
                }
                return Ok(());
            }
            "message_update" => {
                let update = &record["assistantMessageEvent"];
                let delta = update["delta"].as_str().unwrap_or_default().to_string();
                match update["type"].as_str() {
                    Some("text_delta") => events(ExternalEvent::Text(delta)),
                    Some("thinking_delta") => events(ExternalEvent::Reasoning(delta)),
                    _ => {}
                }
            }
            "tool_execution_start" if record["parentToolCallId"].is_null() => {
                events(ExternalEvent::ToolStarted {
                    id: text(&record["toolCallId"]),
                    title: tool_title(&text(&record["toolName"]), &record["args"]),
                });
            }
            "tool_execution_end" if record["parentToolCallId"].is_null() => {
                let result = record["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap_or_default();
                if result.starts_with("Glyphst keeps this agent's files") {
                    events(ExternalEvent::Refused(tool_title(
                        &text(&record["toolName"]),
                        &record["args"],
                    )));
                }
                events(ExternalEvent::ToolFinished {
                    id: text(&record["toolCallId"]),
                    failed: record["isError"].as_bool().unwrap_or(false),
                });
            }
            "agent_settled" if !settled => {
                settled = true;
                send(&mut process, json!({"id": "state", "type": "get_state"}))?;
            }
            _ => {}
        }
    }
}

fn send(process: &mut AgentProcess, record: Value) -> Result<()> {
    let stdin = process.stdin.as_mut().context("Pi's stdin is closed")?;
    writeln!(stdin, "{record}").context("could not write to Pi")?;
    stdin.flush().context("could not write to Pi")
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

/// A short line for a Pi tool call, such as "read notes.md".
fn tool_title(name: &str, args: &Value) -> String {
    let detail = args["path"]
        .as_str()
        .map(|path| {
            std::path::Path::new(path)
                .file_name()
                .map_or(path.to_string(), |name| name.to_string_lossy().into_owned())
        })
        .or_else(|| args["command"].as_str().map(str::to_string))
        .or_else(|| args["pattern"].as_str().map(str::to_string));
    let name = name
        .strip_prefix("mcp__glyphst__")
        .map(|tool| tool.replace('_', " "))
        .unwrap_or_else(|| name.to_string());
    match detail {
        Some(detail) => format!("{name} {}", detail.chars().take(60).collect::<String>()),
        None => name,
    }
}

struct Cleanup(std::path::PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_titles_name_what_the_tool_touches() {
        assert_eq!(
            tool_title("read", &json!({"path": "/x/notes.md"})),
            "read notes.md"
        );
        assert_eq!(tool_title("bash", &json!({"command": "ls"})), "bash ls");
        assert_eq!(
            tool_title("mcp__glyphst__get_outline", &json!({})),
            "get outline"
        );
    }
}
