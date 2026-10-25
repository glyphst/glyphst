//! Claude Code over its stream-json protocol: `claude --print` reads user
//! messages and control responses as JSON lines on stdin and writes events
//! and control requests (tool permissions) on stdout.
//!
//! Every tool is allowed except file writes outside the document's working
//! folder; the Reader's MCP tools are allowed outright.

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
    external::{ExternalEvent, ExternalTurn, inside, instructions},
    process::{AgentProcess, command},
};

/// How long a stopped turn may take to end before the process is stopped.
const INTERRUPT_GRACE: Duration = Duration::from_secs(3);
/// Tools that write files, with the input field naming the file.
const WRITING_TOOLS: [(&str, &str); 4] = [
    ("Write", "file_path"),
    ("Edit", "file_path"),
    ("MultiEdit", "file_path"),
    ("NotebookEdit", "notebook_path"),
];

pub fn run_turn(
    turn: &ExternalTurn,
    cancel: &AgentCancellationToken,
    events: &mut dyn FnMut(ExternalEvent),
) -> Result<()> {
    let config = McpConfigFile::write(turn)?;
    let mut command = command(&turn.agent.program, &turn.scratch.dir);
    command.args([
        "--print",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-prompt-tool",
        "stdio",
        "--permission-mode",
        "default",
        "--allowedTools",
        "mcp__glyphst",
        "--mcp-config",
    ]);
    command.arg(&config.path);
    command
        .arg("--append-system-prompt")
        .arg(instructions(&turn.scratch));
    if let Some(model) = &turn.model {
        command.args(["--model", model]);
    }
    if let Some(session) = &turn.resume {
        command.arg(format!("--resume={session}"));
    }
    let mut process = AgentProcess::spawn(command, "Claude Code")?;
    write_line(&mut process, &prompt_line(turn))?;

    let mut interrupted: Option<Instant> = None;
    loop {
        if cancel.is_canceled() && interrupted.is_none() {
            let _ = write_line(
                &mut process,
                &json!({
                    "type": "control_request",
                    "request_id": "glyphst-interrupt",
                    "request": {"subtype": "interrupt"},
                })
                .to_string(),
            );
            interrupted = Some(Instant::now());
        }
        if interrupted.is_some_and(|at| at.elapsed() > INTERRUPT_GRACE) {
            process.stop();
            bail!("agent run canceled");
        }
        let line = match process.lines.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                if interrupted.is_some() {
                    bail!("agent run canceled");
                }
                return Err(process.exit_error());
            }
        };
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match frame["type"].as_str() {
            Some("system") if frame["subtype"] == "init" => {
                if let Some(session) = frame["session_id"].as_str() {
                    events(ExternalEvent::Session(session.to_string()));
                }
            }
            Some("stream_event") if frame["parent_tool_use_id"].is_null() => {
                let delta = &frame["event"]["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        events(ExternalEvent::Text(text(&delta["text"])));
                    }
                    Some("thinking_delta") => {
                        events(ExternalEvent::Reasoning(text(&delta["thinking"])));
                    }
                    _ => {}
                }
            }
            Some("assistant") if frame["parent_tool_use_id"].is_null() => {
                for block in blocks(&frame) {
                    if block["type"] == "tool_use" {
                        events(ExternalEvent::ToolStarted {
                            id: text(&block["id"]),
                            title: tool_title(&text(&block["name"]), &block["input"]),
                        });
                    }
                }
            }
            Some("user") => {
                for block in blocks(&frame) {
                    if block["type"] == "tool_result" {
                        events(ExternalEvent::ToolFinished {
                            id: text(&block["tool_use_id"]),
                            failed: block["is_error"].as_bool().unwrap_or(false),
                        });
                    }
                }
            }
            Some("control_request") => {
                let response = permission(turn, &frame["request"], events);
                write_line(
                    &mut process,
                    &json!({
                        "type": "control_response",
                        "response": {
                            "subtype": "success",
                            "request_id": frame["request_id"],
                            "response": response,
                        },
                    })
                    .to_string(),
                )?;
            }
            Some("result") => {
                if let Some(session) = frame["session_id"].as_str() {
                    events(ExternalEvent::Session(session.to_string()));
                }
                let failed = frame["is_error"].as_bool().unwrap_or(false)
                    || frame["subtype"]
                        .as_str()
                        .is_some_and(|kind| kind != "success");
                process.finish();
                if interrupted.is_some() {
                    bail!("agent run canceled");
                }
                if failed {
                    let reason = frame["result"]
                        .as_str()
                        .or_else(|| frame["subtype"].as_str())
                        .unwrap_or("the turn failed");
                    return Err(anyhow!("Claude Code: {reason}"));
                }
                return Ok(());
            }
            _ => {}
        }
    }
}

/// The answer to a `can_use_tool` request: everything is allowed except
/// writing files outside the working folder.
fn permission(
    turn: &ExternalTurn,
    request: &Value,
    events: &mut dyn FnMut(ExternalEvent),
) -> Value {
    if request["subtype"] != "can_use_tool" {
        return json!({});
    }
    let tool = request["tool_name"].as_str().unwrap_or_default();
    let input = &request["input"];
    if let Some((_, field)) = WRITING_TOOLS.iter().find(|(name, _)| *name == tool)
        && let Some(path) = input[*field].as_str()
        && !inside(&turn.scratch, path)
    {
        let message = format!(
            "Glyphst keeps this agent's files in its working folder {}; write there instead of {path}.",
            turn.scratch.dir.display()
        );
        events(ExternalEvent::Refused(format!("{tool} {path}")));
        return json!({"behavior": "deny", "message": message});
    }
    json!({"behavior": "allow", "updatedInput": input})
}

fn prompt_line(turn: &ExternalTurn) -> String {
    let mut content = turn
        .images
        .iter()
        .map(|image| {
            json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": image.media_type,
                    "data": BASE64.encode(image.bytes.as_ref()),
                },
            })
        })
        .collect::<Vec<_>>();
    content.push(json!({"type": "text", "text": turn.prompt}));
    json!({
        "type": "user",
        "message": {"role": "user", "content": content},
        "parent_tool_use_id": null,
    })
    .to_string()
}

fn write_line(process: &mut AgentProcess, line: &str) -> Result<()> {
    let stdin = process
        .stdin
        .as_mut()
        .context("the agent's stdin is closed")?;
    writeln!(stdin, "{line}").context("could not write to the agent")?;
    stdin.flush().context("could not write to the agent")
}

fn blocks(frame: &Value) -> impl Iterator<Item = &Value> {
    frame["message"]["content"].as_array().into_iter().flatten()
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

/// A short line for a tool call, such as "Read notes.md".
pub fn tool_title(name: &str, input: &Value) -> String {
    let field = |key: &str| input[key].as_str().map(str::to_string);
    let file = |key: &str| {
        field(key).map(|path| {
            std::path::Path::new(&path)
                .file_name()
                .map_or(path.clone(), |name| name.to_string_lossy().into_owned())
        })
    };
    let detail = match name {
        "Read" | "Write" | "Edit" | "MultiEdit" => file("file_path"),
        "NotebookEdit" => file("notebook_path"),
        "Bash" => field("description").or_else(|| field("command")),
        "Grep" | "Glob" => field("pattern"),
        "WebFetch" => field("url"),
        "WebSearch" => field("query"),
        _ => None,
    };
    let name = name
        .strip_prefix("mcp__glyphst__")
        .map(|tool| tool.replace('_', " "))
        .unwrap_or_else(|| name.to_string());
    match detail {
        Some(detail) => format!("{name} {}", detail.chars().take(60).collect::<String>()),
        None => name,
    }
}

/// The MCP config, in a file only the user can read (a token in argv
/// would be visible to every process).
struct McpConfigFile {
    path: std::path::PathBuf,
}

impl McpConfigFile {
    fn write(turn: &ExternalTurn) -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "glyphst-mcp-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let config = json!({
            "mcpServers": {
                "glyphst": {
                    "type": "http",
                    "url": turn.mcp.url,
                    "headers": {"Authorization": format!("Bearer {}", turn.mcp.token)},
                }
            }
        });
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options
            .open(&path)
            .context("could not write the MCP config")?;
        file.write_all(config.to_string().as_bytes())?;
        Ok(Self { path })
    }
}

impl Drop for McpConfigFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_titles_name_what_the_tool_touches() {
        assert_eq!(
            tool_title("Read", &json!({"file_path": "/a/b/notes.md"})),
            "Read notes.md"
        );
        assert_eq!(
            tool_title(
                "Bash",
                &json!({"command": "ls", "description": "List files"})
            ),
            "Bash List files"
        );
        assert_eq!(
            tool_title("mcp__glyphst__read_document", &json!({})),
            "read document"
        );
    }
}
