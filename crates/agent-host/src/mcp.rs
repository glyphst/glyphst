//! An MCP server (streamable HTTP, protocol 2025-06-18 and 2025-03-26) on
//! 127.0.0.1 that serves the Reader's tools to external agents.
//!
//! Each agent session gets its own bearer token, which names the session's
//! tools and the conversation their changes belong to. Requests from a web
//! page (an `Origin` that is not local) are refused, against DNS rebinding.
//! Responses are plain JSON; the server opens no event streams.

use std::{
    collections::HashMap,
    io::Read as _,
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use glyphst_ai::{
    agent::{AgentToolCall, AgentToolResultPart},
    tools::{ToolContext, ToolEffect, ToolRegistry},
};
use serde_json::{Value, json};

const PROTOCOL_VERSIONS: [&str; 2] = ["2025-06-18", "2025-03-26"];
/// Request bodies larger than this are refused.
const MAX_BODY: u64 = 4 * 1024 * 1024;
const INSTRUCTIONS: &str = "Tools of Glyphst Reader, the PDF and EPUB reader the user is reading in. Call get_app_state or list_documents first to learn the open documents and their ids; read and search them with read_document, search_document and render_document_pages; changes you make (highlights, notes, bookmarks, the library) show in the reader at once, and the user can undo them.";

/// The tools and context of one agent session.
#[derive(Clone)]
pub struct McpSession {
    pub tools: Arc<ToolRegistry>,
    pub context: ToolContext,
}

/// The sessions the server serves, by token.
#[derive(Clone, Default)]
pub struct McpSessions(Arc<Mutex<HashMap<String, McpSession>>>);

impl McpSessions {
    /// Registers a session; returns its bearer token.
    pub fn open(&self, session: McpSession) -> String {
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        if let Ok(mut sessions) = self.0.lock() {
            sessions.insert(token.clone(), session);
        }
        token
    }

    pub fn close(&self, token: &str) {
        if let Ok(mut sessions) = self.0.lock() {
            sessions.remove(token);
        }
    }

    fn get(&self, token: &str) -> Option<McpSession> {
        self.0.lock().ok()?.get(token).cloned()
    }
}

/// A running server.
pub struct McpServer {
    pub url: String,
    pub sessions: McpSessions,
}

impl McpServer {
    /// Serves `sessions` on a free port of 127.0.0.1 until the process ends.
    pub fn start(sessions: McpSessions) -> Result<Self> {
        let server = tiny_http::Server::http("127.0.0.1:0").map_err(|error| anyhow!("{error}"))?;
        let port = server
            .server_addr()
            .to_ip()
            .map(|address| address.port())
            .ok_or_else(|| anyhow!("the MCP server has no port"))?;
        let served = sessions.clone();
        thread::Builder::new()
            .name("glyphst-mcp".into())
            .spawn(move || {
                for request in server.incoming_requests() {
                    let sessions = served.clone();
                    // Tool calls can wait for the app, so each has a thread.
                    let _ = thread::Builder::new()
                        .name("glyphst-mcp-request".into())
                        .spawn(move || respond(request, &sessions));
                }
            })?;
        Ok(Self {
            url: format!("http://127.0.0.1:{port}/mcp"),
            sessions,
        })
    }
}

fn respond(mut request: tiny_http::Request, sessions: &McpSessions) {
    let header = |name: &str| {
        request
            .headers()
            .iter()
            .find(|header| header.field.as_str().as_str().eq_ignore_ascii_case(name))
            .map(|header| header.value.as_str().to_string())
    };
    let reply = |status: u16, body: Option<Value>| {
        let mut response =
            tiny_http::Response::from_string(body.map(|body| body.to_string()).unwrap_or_default())
                .with_status_code(status);
        if let Ok(header) = "Content-Type: application/json".parse::<tiny_http::Header>() {
            response.add_header(header);
        }
        response
    };
    if let Some(origin) = header("Origin")
        && !local_origin(&origin)
    {
        let _ = request.respond(reply(403, None));
        return;
    }
    if !request.url().starts_with("/mcp") {
        let _ = request.respond(reply(404, None));
        return;
    }
    let session = header("Authorization")
        .and_then(|value| value.strip_prefix("Bearer ").map(str::to_string))
        .and_then(|token| sessions.get(token.trim()));
    let Some(session) = session else {
        let _ = request.respond(reply(401, None));
        return;
    };
    match request.method() {
        tiny_http::Method::Post => {}
        // Sessions end with the agent; there is no event stream to open.
        tiny_http::Method::Delete => {
            let _ = request.respond(reply(200, None));
            return;
        }
        _ => {
            let _ = request.respond(reply(405, None));
            return;
        }
    }
    let mut body = String::new();
    if request
        .as_reader()
        .take(MAX_BODY + 1)
        .read_to_string(&mut body)
        .is_err()
        || body.len() as u64 > MAX_BODY
    {
        let _ = request.respond(reply(413, None));
        return;
    }
    let message: Value = match serde_json::from_str(&body) {
        Ok(message) => message,
        Err(error) => {
            let error = rpc_error(Value::Null, -32700, &format!("parse error: {error}"));
            let _ = request.respond(reply(400, Some(error)));
            return;
        }
    };
    let answers = match message {
        Value::Array(batch) => {
            let answers = batch
                .into_iter()
                .filter_map(|message| handle(&message, &session))
                .collect::<Vec<_>>();
            (!answers.is_empty()).then_some(Value::Array(answers))
        }
        message => handle(&message, &session),
    };
    let session_header = (message_is_initialize(&body))
        .then(|| format!("Mcp-Session-Id: {}", uuid::Uuid::new_v4().simple()))
        .and_then(|header| header.parse::<tiny_http::Header>().ok());
    let mut response = match answers {
        Some(answers) => reply(200, Some(answers)),
        // Notifications and responses are accepted without a body.
        None => reply(202, None),
    };
    if let Some(header) = session_header {
        response.add_header(header);
    }
    let _ = request.respond(response);
}

fn message_is_initialize(body: &str) -> bool {
    serde_json::from_str::<Value>(body).is_ok_and(|message| message["method"] == "initialize")
}

/// Whether an `Origin` header names this machine.
fn local_origin(origin: &str) -> bool {
    let host = origin
        .split("://")
        .nth(1)
        .unwrap_or(origin)
        .split('/')
        .next()
        .unwrap_or_default();
    let host = host
        .rsplit_once(':')
        .filter(|(_, port)| port.chars().all(|ch| ch.is_ascii_digit()))
        .map_or(host, |(host, _)| host);
    matches!(host, "localhost" | "127.0.0.1" | "[::1]") || origin == "null"
}

/// The answer to one JSON-RPC message; `None` for notifications.
fn handle(message: &Value, session: &McpSession) -> Option<Value> {
    let id = message.get("id").cloned()?;
    let method = message["method"].as_str().unwrap_or_default();
    let params = &message["params"];
    Some(match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or_default();
            let version = PROTOCOL_VERSIONS
                .into_iter()
                .find(|version| *version == asked)
                .unwrap_or(PROTOCOL_VERSIONS[0]);
            rpc_result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "glyphst", "title": "Glyphst Reader", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }),
            )
        }
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(id, json!({"tools": tool_list(&session.tools)})),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            if session.tools.get(name).is_none() {
                return Some(rpc_error(id, -32602, &format!("unknown tool: {name}")));
            }
            let call = AgentToolCall {
                id: format!("mcp-{}", uuid::Uuid::new_v4().simple()),
                name: name.to_string(),
                arguments: params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}))
                    .to_string(),
            };
            let result = session.tools.execute(&call, &session.context);
            let content = result
                .content
                .iter()
                .map(|part| match part {
                    AgentToolResultPart::Text(text) => json!({"type": "text", "text": text}),
                    AgentToolResultPart::Image(image) => json!({
                        "type": "image",
                        "data": BASE64.encode(image.bytes.as_ref()),
                        "mimeType": image.media_type,
                    }),
                })
                .collect::<Vec<_>>();
            rpc_result(id, json!({"content": content, "isError": result.is_error}))
        }
        "resources/list" => rpc_result(id, json!({"resources": []})),
        "prompts/list" => rpc_result(id, json!({"prompts": []})),
        other => rpc_error(id, -32601, &format!("method not found: {other}")),
    })
}

fn tool_list(tools: &ToolRegistry) -> Vec<Value> {
    tools
        .definitions()
        .into_iter()
        .map(|definition| {
            let effect = tools.effect(&definition.name);
            json!({
                "name": definition.name,
                "description": definition.description,
                "inputSchema": definition.input_schema,
                "annotations": {
                    "readOnlyHint": effect == ToolEffect::Read,
                    "destructiveHint": effect == ToolEffect::Mutate,
                    "idempotentHint": effect == ToolEffect::Read,
                    "openWorldHint": tools.open_world(&definition.name),
                },
            })
        })
        .collect()
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

#[cfg(test)]
mod tests {
    use glyphst_ai::{
        agent::{AgentImage, AgentToolDefinition, AgentToolResult},
        tools::{Tool, ToolOrigin},
    };

    use super::*;

    struct Page;

    impl Tool for Page {
        fn definition(&self) -> AgentToolDefinition {
            AgentToolDefinition::new("render_page", "Render a page.", json!({"type": "object"}))
        }

        fn effect(&self) -> ToolEffect {
            ToolEffect::Read
        }

        fn call(&self, call: &AgentToolCall, cx: &ToolContext) -> AgentToolResult {
            assert_eq!(cx.origin, ToolOrigin::External("Test agent".into()));
            AgentToolResult {
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                content: vec![
                    AgentToolResultPart::Text(format!("asked {}", call.arguments)),
                    AgentToolResultPart::Image(AgentImage::new("image/png", 1, 1, vec![1, 2])),
                ],
                is_error: false,
            }
        }
    }

    fn post(url: &str, token: Option<&str>, origin: Option<&str>, body: Value) -> (u16, Value) {
        let client = reqwest::blocking::Client::new();
        let mut request = client.post(url).json(&body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(origin) = origin {
            request = request.header("Origin", origin);
        }
        let response = request.send().unwrap();
        let status = response.status().as_u16();
        let text = response.text().unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    #[test]
    fn agents_list_and_call_tools_with_their_token() {
        let sessions = McpSessions::default();
        let server = McpServer::start(sessions.clone()).unwrap();
        let token = sessions.open(McpSession {
            tools: Arc::new(ToolRegistry::new().with(Page)),
            context: ToolContext {
                origin: ToolOrigin::External("Test agent".into()),
                ..ToolContext::default()
            },
        });
        let rpc = |id: u64, method: &str, params: Value| json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});

        let (status, init) = post(
            &server.url,
            Some(&token),
            None,
            rpc(1, "initialize", json!({"protocolVersion": "2025-03-26"})),
        );
        assert_eq!(status, 200);
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "glyphst");
        let (status, _) = post(
            &server.url,
            Some(&token),
            None,
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        );
        assert_eq!(status, 202);

        let (_, list) = post(
            &server.url,
            Some(&token),
            None,
            rpc(2, "tools/list", json!({})),
        );
        let tool = &list["result"]["tools"][0];
        assert_eq!(tool["name"], "render_page");
        assert_eq!(tool["annotations"]["readOnlyHint"], true);

        let (_, called) = post(
            &server.url,
            Some(&token),
            None,
            rpc(
                3,
                "tools/call",
                json!({"name": "render_page", "arguments": {"page": 2}}),
            ),
        );
        let content = &called["result"]["content"];
        assert_eq!(content[0]["text"], r#"asked {"page":2}"#);
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["data"], "AQI=");
        assert_eq!(called["result"]["isError"], false);

        let (_, unknown) = post(
            &server.url,
            Some(&token),
            None,
            rpc(4, "tools/call", json!({"name": "delete_everything"})),
        );
        assert_eq!(unknown["error"]["code"], -32602);

        // Without the token, or from a web page, nothing is served.
        assert_eq!(
            post(&server.url, None, None, rpc(5, "tools/list", json!({}))).0,
            401
        );
        assert_eq!(
            post(
                &server.url,
                Some("wrong"),
                None,
                rpc(5, "tools/list", json!({}))
            )
            .0,
            401
        );
        assert_eq!(
            post(
                &server.url,
                Some(&token),
                Some("https://evil.example"),
                rpc(5, "tools/list", json!({}))
            )
            .0,
            403
        );
        assert_eq!(
            post(
                &server.url,
                Some(&token),
                Some("http://localhost:3000"),
                rpc(5, "ping", json!({}))
            )
            .0,
            200
        );
        sessions.close(&token);
        assert_eq!(
            post(&server.url, Some(&token), None, rpc(6, "ping", json!({}))).0,
            401
        );
    }
}
