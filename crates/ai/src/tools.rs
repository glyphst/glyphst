//! Tools the agent can call, and the registry a run offers.
//!
//! Each tool says what it does to the app ([`ToolEffect`]): reads run in
//! parallel, navigation and changes run in the order the model asked for
//! them, and only changes are recorded for undo by the app.

use std::sync::Arc;

use serde_json::Value;

use crate::agent::{AgentCancellationToken, AgentToolCall, AgentToolDefinition, AgentToolResult};

/// What a tool call does to the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolEffect {
    /// Reads documents or app state; safe to run alongside other reads.
    Read,
    /// Moves the view (pages, selection, tabs); not undone.
    Navigate,
    /// Changes saved data (annotations, the library); undoable.
    Mutate,
}

/// Who asked for a tool call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ToolOrigin {
    /// The built-in assistant.
    #[default]
    Builtin,
    /// An external agent (Claude Code, Codex, …) through the MCP server.
    External(String),
}

/// What a call knows about its run.
#[derive(Clone, Default)]
pub struct ToolContext {
    pub cancel: AgentCancellationToken,
    pub origin: ToolOrigin,
    /// The conversation the call belongs to, for the app's change log.
    pub thread: Option<u64>,
    /// The user turn the call belongs to, from 0.
    pub turn: usize,
}

pub trait Tool: Send + Sync + 'static {
    fn definition(&self) -> AgentToolDefinition;

    fn effect(&self) -> ToolEffect;

    fn call(&self, call: &AgentToolCall, cx: &ToolContext) -> AgentToolResult;

    /// A short line for the call in the chat, such as "Read pages 3-5".
    fn describe(&self, _arguments: &Value) -> Option<String> {
        None
    }

    /// Whether it reaches outside the app (the network), for MCP's hints.
    fn open_world(&self) -> bool {
        false
    }
}

/// The tools of a run, in the order they are offered.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, tool: impl Tool) -> Self {
        self.register(tool);
        self
    }

    /// Adds a tool; a later tool replaces an earlier one with the same name.
    pub fn register(&mut self, tool: impl Tool) {
        let name = tool.definition().name;
        self.tools
            .retain(|existing| existing.definition().name != name);
        self.tools.push(Arc::new(tool));
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn definitions(&self) -> Vec<AgentToolDefinition> {
        self.tools.iter().map(|tool| tool.definition()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools
            .iter()
            .find(|tool| tool.definition().name == name)
    }

    /// Unknown tools count as changes, so they never run out of order.
    pub fn effect(&self, name: &str) -> ToolEffect {
        self.get(name)
            .map_or(ToolEffect::Mutate, |tool| tool.effect())
    }

    pub fn open_world(&self, name: &str) -> bool {
        self.get(name).is_some_and(|tool| tool.open_world())
    }

    pub fn execute(&self, call: &AgentToolCall, cx: &ToolContext) -> AgentToolResult {
        match self.get(&call.name) {
            Some(tool) => tool.call(call, cx),
            None => AgentToolResult::error(
                call.id.clone(),
                call.name.clone(),
                format!("unknown tool: {}", call.name),
            ),
        }
    }

    /// The chat line for a call, falling back to the tool's name.
    pub fn describe(&self, name: &str, arguments: &str) -> String {
        let arguments = serde_json::from_str::<Value>(arguments).unwrap_or(Value::Null);
        self.get(name)
            .and_then(|tool| tool.describe(&arguments))
            .unwrap_or_else(|| name.replace('_', " "))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    struct Echo(&'static str, ToolEffect);

    impl Tool for Echo {
        fn definition(&self) -> AgentToolDefinition {
            AgentToolDefinition::new(self.0, "Echo.", json!({"type": "object"}))
        }

        fn effect(&self) -> ToolEffect {
            self.1
        }

        fn call(&self, call: &AgentToolCall, _cx: &ToolContext) -> AgentToolResult {
            AgentToolResult::text(&call.id, &call.name, call.arguments.clone())
        }

        fn describe(&self, arguments: &Value) -> Option<String> {
            arguments["what"]
                .as_str()
                .map(|what| format!("Echo {what}"))
        }
    }

    #[test]
    fn registry_finds_runs_and_describes_tools() {
        let registry = ToolRegistry::new()
            .with(Echo("read", ToolEffect::Read))
            .with(Echo("change", ToolEffect::Mutate))
            .with(Echo("read", ToolEffect::Read));
        assert_eq!(registry.definitions().len(), 2);
        assert_eq!(registry.effect("read"), ToolEffect::Read);
        assert_eq!(registry.effect("missing"), ToolEffect::Mutate);
        assert_eq!(registry.describe("read", r#"{"what":"it"}"#), "Echo it");
        assert_eq!(registry.describe("go_to_page", "{}"), "go to page");

        let call = AgentToolCall {
            id: "1".into(),
            name: "missing".into(),
            arguments: "{}".into(),
        };
        assert!(registry.execute(&call, &ToolContext::default()).is_error);
    }
}
