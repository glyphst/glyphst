//! A stand-in for the `claude` and `codex` CLIs in the driver tests: it
//! speaks enough of their protocols, calls the Reader's MCP server, asks to
//! write a file outside and inside its working folder, and reports what it
//! saw in its answer.

use std::io::{BufRead as _, Write as _};

use serde_json::{Value, json};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "app-server") {
        codex();
    } else {
        claude(&args);
    }
}

fn say(value: Value) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{value}");
    let _ = stdout.flush();
}

/// The number of tools the MCP server lists.
fn mcp_tools(url: &str, token: &str) -> usize {
    let answer: Value = reqwest::blocking::Client::new()
        .post(url)
        .bearer_auth(token)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .send()
        .and_then(|response| response.json())
        .unwrap_or_default();
    answer["result"]["tools"].as_array().map_or(0, Vec::len)
}

fn claude(args: &[String]) {
    let config_path = args
        .iter()
        .position(|arg| arg == "--mcp-config")
        .and_then(|index| args.get(index + 1))
        .expect("--mcp-config");
    let config: Value =
        serde_json::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap();
    let server = &config["mcpServers"]["glyphst"];
    let token = server["headers"]["Authorization"]
        .as_str()
        .unwrap()
        .trim_start_matches("Bearer ");
    let resumed = args.iter().any(|arg| arg.starts_with("--resume="));
    let mut lines = std::io::stdin().lock().lines();
    let message: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    let prompt = message["message"]["content"]
        .as_array()
        .and_then(|content| content.iter().find(|block| block["type"] == "text"))
        .and_then(|block| block["text"].as_str())
        .unwrap_or_default()
        .to_string();
    say(json!({"type": "system", "subtype": "init", "session_id": "fake-session"}));
    let tools = mcp_tools(server["url"].as_str().unwrap(), token);

    let mut ask = |id: &str, path: String| -> bool {
        say(json!({
            "type": "control_request",
            "request_id": id,
            "request": {"subtype": "can_use_tool", "tool_name": "Write", "input": {"file_path": path, "content": "x"}},
        }));
        let answer: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        answer["response"]["response"]["behavior"] == "allow"
    };
    let outside = ask("1", "/tmp/glyphst-fake-outside.txt".into());
    let cwd = std::env::current_dir().unwrap();
    let inside = ask("2", cwd.join("notes.md").display().to_string());
    if inside {
        std::fs::write(cwd.join("notes.md"), "written by the fake agent\n").unwrap();
    }
    say(
        json!({"type": "assistant", "parent_tool_use_id": null, "message": {"content": [
            {"type": "tool_use", "id": "tool-1", "name": "Write", "input": {"file_path": "notes.md"}}
        ]}}),
    );
    say(json!({"type": "user", "message": {"content": [
        {"type": "tool_result", "tool_use_id": "tool-1", "is_error": false}
    ]}}));
    for text in [
        format!("tools={tools} "),
        format!("outside={outside} inside={inside} resumed={resumed} "),
        format!("prompt={}", prompt.lines().next().unwrap_or_default()),
    ] {
        say(
            json!({"type": "stream_event", "parent_tool_use_id": null, "event": {
                "type": "content_block_delta", "delta": {"type": "text_delta", "text": text}
            }}),
        );
    }
    say(
        json!({"type": "result", "subtype": "success", "is_error": false, "session_id": "fake-session"}),
    );
    // Like the CLI, wait for stdin to close.
    for _ in lines {}
}

fn codex() {
    let token = std::env::var("GLYPHST_MCP_TOKEN").unwrap_or_default();
    let mut url = String::new();
    for line in std::io::stdin().lock().lines() {
        let message: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id = message["id"].clone();
        let params = &message["params"];
        match message["method"].as_str().unwrap_or_default() {
            "initialize" => {
                say(json!({"jsonrpc": "2.0", "id": id, "result": {"userAgent": "fake"}}))
            }
            "thread/resume" => say(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -1, "message": "no such thread"}}),
            ),
            "thread/start" => {
                url = params["config"]["mcp_servers.glyphst.url"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert_eq!(params["sandbox"], "workspace-write");
                assert_eq!(params["approvalPolicy"], "never");
                say(
                    json!({"jsonrpc": "2.0", "id": id, "result": {"thread": {"id": "fake-thread"}}}),
                );
            }
            "turn/start" => {
                say(json!({"jsonrpc": "2.0", "id": id, "result": {"turn": {"id": "turn-1"}}}));
                let tools = mcp_tools(&url, &token);
                std::fs::write("notes.md", "written by fake codex\n").unwrap();
                let item = json!({"type": "fileChange", "id": "item-1", "status": "completed", "changes": [{"path": "notes.md"}]});
                say(json!({"jsonrpc": "2.0", "method": "item/started", "params": {"item": item}}));
                say(
                    json!({"jsonrpc": "2.0", "method": "item/completed", "params": {"item": item}}),
                );
                // An approval request despite "never".
                say(
                    json!({"jsonrpc": "2.0", "id": 99, "method": "item/commandExecution/requestApproval", "params": {}}),
                );
                let prompt = params["input"][0]["text"].as_str().unwrap_or_default();
                let answer = format!(
                    "tools={tools} prompt={}",
                    prompt.lines().next().unwrap_or_default()
                );
                for text in ["Looking.", answer.as_str()] {
                    say(
                        json!({"jsonrpc": "2.0", "method": "item/started", "params": {"item": {"type": "agentMessage", "id": "message", "text": ""}}}),
                    );
                    say(
                        json!({"jsonrpc": "2.0", "method": "item/agentMessage/delta", "params": {"delta": text}}),
                    );
                }
            }
            "" if id == json!(99) => {
                // The approval's answer: finish the turn.
                assert_eq!(message["result"]["decision"], "accept");
                say(
                    json!({"jsonrpc": "2.0", "method": "turn/completed", "params": {"turn": {"id": "turn-1", "status": "completed", "items": []}}}),
                );
            }
            _ => {}
        }
    }
}
