//! The drivers against a fake agent CLI that calls the MCP server back.

use std::{path::PathBuf, sync::Arc};

use glyphst_agent_host::{
    external::{AgentKind, AgentSpec, ExternalEvent, ExternalTurn, McpAccess, run_turn},
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

/// A turn in a folder of its own: `test` names it, as tests run at once and
/// two may drive the same agent.
fn turn(kind: AgentKind, test: &str) -> (ExternalTurn, PathBuf) {
    let sessions = McpSessions::default();
    let server = McpServer::start(sessions.clone()).unwrap();
    let token = sessions.open(McpSession {
        tools: Arc::new(ToolRegistry::new().with(Info)),
        context: ToolContext::default(),
    });
    let root = std::env::temp_dir().join(format!("glyphst-drivers-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let scratch = Scratch::open(&root, "doc", "Doc").unwrap();
    let mut agent = AgentSpec::known(
        kind,
        PathBuf::from(env!("CARGO_BIN_EXE_glyphst-fake-agent")),
    );
    if kind == AgentKind::Acp {
        agent.name = "Fake ACP".into();
        agent.args = vec!["acp".into()];
    }
    let turn = ExternalTurn {
        agent,
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

fn run(turn: &ExternalTurn) -> Vec<ExternalEvent> {
    let mut events = Vec::new();
    run_turn(turn, &AgentCancellationToken::default(), &mut |event| {
        events.push(event)
    })
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
    let (mut turn, root) = turn(AgentKind::ClaudeCode, "claude-code");
    let events = run(&turn);
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
    assert!(text(&run(&turn)).contains("resumed=true"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn codex_turns_use_the_mcp_server_and_resume_or_start_threads() {
    let (mut turn, root) = turn(AgentKind::Codex, "codex");
    // A thread that cannot be resumed starts afresh.
    turn.resume = Some("gone".into());
    let events = run(&turn);
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
fn pi_turns_load_the_extension_and_report_their_session() {
    let (mut turn, root) = turn(AgentKind::Pi, "pi");
    let events = run(&turn);
    assert!(events.contains(&ExternalEvent::Session("pi-session".into())));
    assert_eq!(
        text(&events),
        "tools=1 resumed=false prompt=Summarize page 2"
    );
    assert!(events.contains(&ExternalEvent::ToolStarted {
        id: "call-1".into(),
        title: "write notes.md".into()
    }));
    assert!(turn.scratch.dir.join("notes.md").exists());
    turn.resume = Some("pi-session".into());
    assert!(text(&run(&turn)).contains("resumed=true"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn acp_agents_get_the_mcp_server_and_permissions_once() {
    let (mut turn, root) = turn(AgentKind::Acp, "acp");
    let events = run(&turn);
    assert!(events.contains(&ExternalEvent::Session("acp-session".into())));
    assert_eq!(text(&events), "tools=1 permission=yes last=</reader_state>");
    assert!(events.contains(&ExternalEvent::ToolFinished {
        id: "tool-1".into(),
        failed: false
    }));
    assert_eq!(turn.agent.id(), "acp:Fake ACP");
    // A loaded session's replayed history is not part of the answer.
    turn.resume = Some("acp-session".into());
    assert!(!text(&run(&turn)).contains("OLD"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_canceled_turn_stops_the_agent() {
    let (turn, root) = turn(AgentKind::ClaudeCode, "canceled");
    let cancel = AgentCancellationToken::default();
    cancel.cancel();
    let started = std::time::Instant::now();
    let result = run_turn(&turn, &cancel, &mut |_| {});
    assert!(result.is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(8));
    let _ = std::fs::remove_dir_all(root);
}
