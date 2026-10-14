//! The drivers against a fake agent CLI that calls the MCP server back.

use std::{path::PathBuf, sync::Arc};

use glyphst_agent_host::{
    external::{AgentKind, ExternalEvent, ExternalTurn, McpAccess, run_turn},
    mcp::{McpServer, McpSession, McpSessions},
    scratch::Scratch,
};
use glyphst_ai::{
    agent::{AgentCancellationToken, AgentToolCall, AgentToolDefinition, AgentToolResult},
    tools::{Tool, ToolContext, ToolEffect, ToolRegistry},
};
use serde_json::json;

struct Info;

impl Tool for Info {
    fn definition(&self) -> AgentToolDefinition {
        AgentToolDefinition::new(
            "get_app_state",
            "The reader's state.",
            json!({"type": "object"}),
        )
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Read
    }

    fn call(&self, call: &AgentToolCall, _cx: &ToolContext) -> AgentToolResult {
        AgentToolResult::text(&call.id, &call.name, "one document")
    }
}

fn turn(kind: AgentKind) -> (ExternalTurn, PathBuf) {
    let sessions = McpSessions::default();
    let server = McpServer::start(sessions.clone()).unwrap();
    let token = sessions.open(McpSession {
        tools: Arc::new(ToolRegistry::new().with(Info)),
        context: ToolContext::default(),
    });
    let root = std::env::temp_dir().join(format!(
        "glyphst-drivers-{}-{}",
        kind.id(),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let scratch = Scratch::open(&root, "doc", "Doc").unwrap();
    let turn = ExternalTurn {
        program: PathBuf::from(env!("CARGO_BIN_EXE_glyphst-fake-agent")),
        scratch,
        mcp: McpAccess {
            url: server.url.clone(),
            token,
        },
        resume: None,
        model: None,
        prompt: "Summarize page 2\n<reader_state>\n</reader_state>".into(),
        images: Vec::new(),
    };
    // The server lives as long as the test.
    std::mem::forget(server);
    (turn, root)
}

fn run(kind: AgentKind, turn: &ExternalTurn) -> Vec<ExternalEvent> {
    let mut events = Vec::new();
    run_turn(
        kind,
        turn,
        &AgentCancellationToken::default(),
        &mut |event| events.push(event),
    )
    .unwrap();
    events
}

fn text(events: &[ExternalEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            ExternalEvent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn claude_code_turns_use_the_mcp_server_and_stay_in_their_folder() {
    let (mut turn, root) = turn(AgentKind::ClaudeCode);
    let events = run(AgentKind::ClaudeCode, &turn);
    assert!(events.contains(&ExternalEvent::Session("fake-session".into())));
    assert_eq!(
        text(&events),
        "tools=1 outside=false inside=true resumed=false prompt=Summarize page 2"
    );
    assert!(events.contains(&ExternalEvent::Refused(
        "Write /tmp/glyphst-fake-outside.txt".into()
    )));
    assert!(events.contains(&ExternalEvent::ToolStarted {
        id: "tool-1".into(),
        title: "Write notes.md".into()
    }));
    assert!(events.contains(&ExternalEvent::ToolFinished {
        id: "tool-1".into(),
        failed: false
    }));
    assert!(turn.scratch.dir.join("notes.md").exists());

    turn.resume = Some("fake-session".into());
    assert!(text(&run(AgentKind::ClaudeCode, &turn)).contains("resumed=true"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn codex_turns_use_the_mcp_server_and_resume_or_start_threads() {
    let (mut turn, root) = turn(AgentKind::Codex);
    // A thread that cannot be resumed starts afresh.
    turn.resume = Some("gone".into());
    let events = run(AgentKind::Codex, &turn);
    assert!(events.contains(&ExternalEvent::Session("fake-thread".into())));
    assert_eq!(text(&events), "Looking.\n\ntools=1 prompt=Summarize page 2");
    assert!(events.contains(&ExternalEvent::ToolStarted {
        id: "item-1".into(),
        title: "Edited notes.md".into()
    }));
    assert!(turn.scratch.dir.join("notes.md").exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_canceled_turn_stops_the_agent() {
    let (turn, root) = turn(AgentKind::ClaudeCode);
    let cancel = AgentCancellationToken::default();
    cancel.cancel();
    let started = std::time::Instant::now();
    let result = run_turn(AgentKind::ClaudeCode, &turn, &cancel, &mut |_| {});
    assert!(result.is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(8));
    let _ = std::fs::remove_dir_all(root);
}
