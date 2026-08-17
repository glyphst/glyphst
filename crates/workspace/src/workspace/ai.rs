use std::{collections::HashMap, sync::Arc};

use anyhow::{Result, anyhow};
use glyphst_ai::{
    agent::{
        AgentCancellationToken, AgentToolCall, AgentToolDefinition, AgentToolExecutor,
        AgentToolResult, AgentToolResultPart,
    },
    ai_chat::{DocumentContextScope, MentionReference},
};
use glyphst_ai_ui::{
    AiContext, AiContextProvider, AiPageState, ResolvedPageContext, ResolvedRegionContext,
};
use glyphst_core::RegionImageReference;
use gpui::{App, Entity, Task, WeakEntity};
use serde_json::{Value, json};

use super::*;
use crate::{document_view::DocumentViewAiContextProvider, library::normalize_path};

#[derive(Clone)]
pub(super) struct WorkspaceAiContextProvider {
    workspace: WeakEntity<Workspace>,
}

#[derive(Clone)]
pub(super) struct WorkspaceAiSnapshot {
    pub(super) current_document: Option<Entity<DocumentView>>,
    pub(super) open_documents: Vec<Entity<DocumentView>>,
}

impl WorkspaceAiContextProvider {
    pub(super) fn new(workspace: WeakEntity<Workspace>) -> Self {
        Self { workspace }
    }

    fn document(
        &self,
        scope: Option<&DocumentContextScope>,
        cx: &App,
    ) -> Option<Entity<DocumentView>> {
        self.workspace
            .read_with(cx, |workspace, cx| {
                scope.map_or_else(
                    || workspace.ai_context_document(cx),
                    |scope| workspace.ai_document_for_path(&scope.path, cx),
                )
            })
            .ok()
            .flatten()
    }

    fn provider(
        &self,
        scope: Option<&DocumentContextScope>,
        cx: &App,
    ) -> Option<DocumentViewAiContextProvider> {
        self.document(scope, cx)
            .map(|document| DocumentViewAiContextProvider::new(&document))
    }

    fn unavailable_page_task(
        references: Vec<MentionReference>,
        cx: &App,
    ) -> Task<Result<Vec<ResolvedPageContext>>> {
        cx.background_spawn(async move {
            if references.is_empty() {
                Ok(Vec::new())
            } else {
                Err(anyhow!(
                    "the source document for this attachment is no longer open"
                ))
            }
        })
    }

    fn unavailable_region_task(
        references: Vec<RegionImageReference>,
        cx: &App,
    ) -> Task<Result<Vec<ResolvedRegionContext>>> {
        cx.background_spawn(async move {
            if references.is_empty() {
                Ok(Vec::new())
            } else {
                Err(anyhow!(
                    "the source document for this attachment is no longer open"
                ))
            }
        })
    }
}

impl AiContextProvider for WorkspaceAiContextProvider {
    fn context(&self, include_document_text: bool, cx: &App) -> Option<AiContext> {
        self.provider(None, cx)?.context(include_document_text, cx)
    }

    fn page_state(&self, cx: &App) -> Option<AiPageState> {
        self.provider(None, cx)?.page_state(cx)
    }

    fn document_scope(&self, cx: &App) -> Option<DocumentContextScope> {
        self.provider(None, cx)?.document_scope(cx)
    }

    fn page_state_for_scope(
        &self,
        scope: Option<&DocumentContextScope>,
        cx: &App,
    ) -> Option<AiPageState> {
        self.provider(scope, cx)?.page_state(cx)
    }

    fn context_for_scope(
        &self,
        scope: Option<&DocumentContextScope>,
        include_document_text: bool,
        cx: &App,
    ) -> Option<AiContext> {
        self.provider(scope, cx)?.context(include_document_text, cx)
    }

    fn resolve_page_references(
        &self,
        references: Vec<MentionReference>,
        cx: &App,
    ) -> Task<Result<Vec<ResolvedPageContext>>> {
        self.resolve_page_references_for_scope(None, references, cx)
    }

    fn resolve_region_references(
        &self,
        references: Vec<RegionImageReference>,
        cx: &App,
    ) -> Task<Result<Vec<ResolvedRegionContext>>> {
        self.resolve_region_references_for_scope(None, references, cx)
    }

    fn resolve_page_references_for_scope(
        &self,
        scope: Option<&DocumentContextScope>,
        references: Vec<MentionReference>,
        cx: &App,
    ) -> Task<Result<Vec<ResolvedPageContext>>> {
        match self.provider(scope, cx) {
            Some(provider) => provider.resolve_page_references(references, cx),
            None => Self::unavailable_page_task(references, cx),
        }
    }

    fn resolve_region_references_for_scope(
        &self,
        scope: Option<&DocumentContextScope>,
        references: Vec<RegionImageReference>,
        cx: &App,
    ) -> Task<Result<Vec<ResolvedRegionContext>>> {
        match self.provider(scope, cx) {
            Some(provider) => provider.resolve_region_references(references, cx),
            None => Self::unavailable_region_task(references, cx),
        }
    }

    fn agent_tools(&self, cx: &App) -> Option<Arc<dyn AgentToolExecutor>> {
        let documents = self
            .workspace
            .read_with(cx, |workspace, cx| workspace.ai_tool_documents(cx))
            .ok()?;
        (!documents.is_empty())
            .then(|| Arc::new(WorkspaceAgentTools { documents }) as Arc<dyn AgentToolExecutor>)
    }
}

pub(super) struct WorkspaceToolDocument {
    id: String,
    label: String,
    format: Option<String>,
    page_count: usize,
    current: bool,
    executor: Arc<dyn AgentToolExecutor>,
    can_render: bool,
}

struct WorkspaceAgentTools {
    documents: Vec<WorkspaceToolDocument>,
}

impl WorkspaceAgentTools {
    fn document(&self, id: &str) -> Option<&WorkspaceToolDocument> {
        self.documents.iter().find(|document| document.id == id)
    }

    fn document_id(arguments: &Value) -> Result<&str> {
        arguments["document_id"]
            .as_str()
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| anyhow!("document_id must identify an open document"))
    }

    fn forwarded_call(call: &AgentToolCall, name: &str, mut arguments: Value) -> AgentToolCall {
        if let Some(arguments) = arguments.as_object_mut() {
            arguments.remove("document_id");
            arguments.remove("document_ids");
        }
        AgentToolCall {
            id: call.id.clone(),
            name: name.to_string(),
            arguments: arguments.to_string(),
        }
    }

    fn list_documents(&self, call: &AgentToolCall) -> AgentToolResult {
        let documents = self
            .documents
            .iter()
            .map(|document| {
                let format = document.format.as_deref().unwrap_or("document");
                let pages = if document.page_count > 0 {
                    format!(", {} pages", document.page_count)
                } else {
                    String::new()
                };
                let current = if document.current {
                    ", current context"
                } else {
                    ""
                };
                format!(
                    "- {}: {} ({format}{pages}{current})",
                    document.id, document.label
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        AgentToolResult::text(
            call.id.clone(),
            call.name.clone(),
            format!("Open documents:\n{documents}"),
        )
    }

    fn search_documents(
        &self,
        call: &AgentToolCall,
        cancellation: &AgentCancellationToken,
    ) -> AgentToolResult {
        let arguments = match call.arguments_json() {
            Ok(arguments) => arguments,
            Err(error) => {
                return AgentToolResult::error(&call.id, &call.name, format!("{error:#}"));
            }
        };
        let selected_ids = arguments["document_ids"]
            .as_array()
            .map(|ids| ids.iter().filter_map(Value::as_str).collect::<Vec<_>>());
        let max_results = arguments["max_results"].as_u64().unwrap_or(12).clamp(1, 40);
        let mut forwarded_arguments = arguments.clone();
        forwarded_arguments["max_results"] = json!(max_results.min(20));
        let forwarded = Self::forwarded_call(call, "search_document", forwarded_arguments);
        let mut output = Vec::new();
        for document in &self.documents {
            if cancellation.is_canceled() {
                return AgentToolResult::error(&call.id, &call.name, "agent run canceled");
            }
            if selected_ids
                .as_ref()
                .is_some_and(|ids| !ids.contains(&document.id.as_str()))
            {
                continue;
            }
            let result = document.executor.execute(&forwarded, cancellation);
            let text = result
                .content
                .into_iter()
                .filter_map(|part| match part {
                    AgentToolResultPart::Text(text) => Some(text),
                    AgentToolResultPart::Image(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            output.push(format!("## {} — {}\n{}", document.id, document.label, text));
        }
        if output.is_empty() {
            return AgentToolResult::error(&call.id, &call.name, "no matching open document ids");
        }
        AgentToolResult::text(&call.id, &call.name, output.join("\n\n"))
    }

    fn execute_for_document(
        &self,
        call: &AgentToolCall,
        delegated_name: &str,
        cancellation: &AgentCancellationToken,
    ) -> AgentToolResult {
        let arguments = match call.arguments_json() {
            Ok(arguments) => arguments,
            Err(error) => {
                return AgentToolResult::error(&call.id, &call.name, format!("{error:#}"));
            }
        };
        let id = match Self::document_id(&arguments) {
            Ok(id) => id,
            Err(error) => {
                return AgentToolResult::error(&call.id, &call.name, format!("{error:#}"));
            }
        };
        let Some(document) = self.document(id) else {
            return AgentToolResult::error(
                &call.id,
                &call.name,
                format!("unknown document_id {id:?}; call list_open_documents again"),
            );
        };
        if delegated_name == "render_document_pages" && !document.can_render {
            return AgentToolResult::error(
                &call.id,
                &call.name,
                format!("{} is not a fixed-layout PDF", document.label),
            );
        }
        let forwarded = Self::forwarded_call(call, delegated_name, arguments);
        let mut result = document.executor.execute(&forwarded, cancellation);
        result.tool_name = call.name.clone();
        result.content.insert(
            0,
            AgentToolResultPart::Text(format!("Document {} — {}", document.id, document.label)),
        );
        for part in &mut result.content {
            if let AgentToolResultPart::Image(image) = part {
                image.name = Some(format!(
                    "{}-{}",
                    document.id,
                    image.name.as_deref().unwrap_or("page.jpg")
                ));
            }
        }
        result
    }
}

impl AgentToolExecutor for WorkspaceAgentTools {
    fn definitions(&self) -> Vec<AgentToolDefinition> {
        let list = AgentToolDefinition::new(
            "list_open_documents",
            "List every fully loaded document in this window. Returns opaque document ids, disambiguated names, formats, and page counts without exposing local paths.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        );
        let mut search = AgentToolDefinition::new(
            "search_documents",
            "Search text across all open documents, or a selected subset, and identify each match by opaque document id and display name.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "document_ids": {"type": "array", "items": {"type": "string"}},
                    "max_results": {"type": "integer", "minimum": 1, "maximum": 40}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        );
        search.strict = false;
        let mut read = AgentToolDefinition::new(
            "read_document",
            "Read exact text from one open document. Use its opaque document_id; for PDFs pass 1-based pages, and for EPUB pass offset/max_chars.",
            json!({
                "type": "object",
                "properties": {
                    "document_id": {"type": "string"},
                    "pages": {"type": "array", "items": {"type": "integer", "minimum": 1}, "maxItems": 12},
                    "start_page": {"type": "integer", "minimum": 1},
                    "end_page": {"type": "integer", "minimum": 1},
                    "offset": {"type": "integer", "minimum": 0},
                    "max_chars": {"type": "integer", "minimum": 1, "maximum": 12000}
                },
                "required": ["document_id"],
                "additionalProperties": false
            }),
        );
        read.strict = false;
        let mut render = AgentToolDefinition::new(
            "render_document_pages",
            "Render 1-based pages from one open fixed-layout PDF using its opaque document_id.",
            json!({
                "type": "object",
                "properties": {
                    "document_id": {"type": "string"},
                    "pages": {"type": "array", "items": {"type": "integer", "minimum": 1}, "minItems": 1}
                },
                "required": ["document_id", "pages"],
                "additionalProperties": false
            }),
        );
        render.strict = false;
        vec![list, search, read, render]
    }

    fn execute(
        &self,
        call: &AgentToolCall,
        cancellation: &AgentCancellationToken,
    ) -> AgentToolResult {
        match call.name.as_str() {
            "list_open_documents" => self.list_documents(call),
            "search_documents" => self.search_documents(call, cancellation),
            "read_document" => self.execute_for_document(call, "read_document", cancellation),
            "render_document_pages" => {
                self.execute_for_document(call, "render_document_pages", cancellation)
            }
            _ => AgentToolResult::error(
                &call.id,
                &call.name,
                format!("unknown workspace tool: {}", call.name),
            ),
        }
    }
}

impl Workspace {
    pub(super) fn toggle_quick_ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quick_ask_visible {
            self.hide_quick_ask(window, cx);
            return;
        }

        // This action already owns the Workspace's mutable GPUI lease. Resolve the
        // initial page state directly instead of reading the Workspace again through
        // WorkspaceAiContextProvider.
        let snapshot = self.ai_context_snapshot(cx);
        let page_state = snapshot
            .current_document
            .as_ref()
            .and_then(|document| DocumentViewAiContextProvider::new(document).page_state(cx))
            .unwrap_or_default();
        let provider = WorkspaceAiContextProvider::new(cx.entity().downgrade());
        let viewport_width = window.viewport_size().width.as_f32();
        let position = cx.global::<SettingsStore>().settings.ai.quick_ask.position;
        let width = match position {
            glyphst_settings::QuickAskPosition::Top
            | glyphst_settings::QuickAskPosition::Bottom => viewport_width,
            glyphst_settings::QuickAskPosition::Left
            | glyphst_settings::QuickAskPosition::Right => 360.0,
            glyphst_settings::QuickAskPosition::Center => {
                (viewport_width - 32.0).clamp(280.0, 640.0)
            }
        };
        let previous_focus = window
            .focused(cx)
            .unwrap_or_else(|| self.focus_handle.clone());
        self.quick_ask_previous_focus = Some(previous_focus.clone());
        let quick_ask = if let Some(quick_ask) = self.quick_ask.clone() {
            quick_ask.update(cx, |panel, cx| {
                panel.prepare_for_show(previous_focus, page_state, width, cx);
            });
            quick_ask
        } else {
            let quick_ask = cx.new(|cx| {
                AiPanel::new_quick_ask(previous_focus, provider, page_state, width, window, cx)
            });
            cx.subscribe_in(
                &quick_ask,
                window,
                |this, _, event: &AiPanelEvent, window, cx| match event {
                    AiPanelEvent::Dismissed => this.dismiss_quick_ask(window, cx),
                    AiPanelEvent::Changed => {}
                    AiPanelEvent::ExternalPathsDropped(paths) => {
                        this.open_external_document_paths(paths, window, cx);
                    }
                },
            )
            .detach();
            self.quick_ask = Some(quick_ask.clone());
            quick_ask
        };
        quick_ask.focus_handle(cx).focus(window, cx);
        self.quick_ask_visible = true;
        cx.notify();
    }

    fn hide_quick_ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quick_ask_visible = false;
        if let Some(previous_focus) = self.quick_ask_previous_focus.take() {
            previous_focus.focus(window, cx);
        }
        cx.notify();
    }

    fn dismiss_quick_ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quick_ask = None;
        self.hide_quick_ask(window, cx);
    }

    pub(super) fn render_quick_ask_overlay(
        &self,
        quick_ask: Entity<AiPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use glyphst_settings::QuickAskPosition;

        let position = cx.global::<SettingsStore>().settings.ai.quick_ask.position;
        let viewport = window.viewport_size();
        let width = match position {
            QuickAskPosition::Top | QuickAskPosition::Bottom => viewport.width.as_f32(),
            QuickAskPosition::Left | QuickAskPosition::Right => 360.0,
            QuickAskPosition::Center => (viewport.width.as_f32() - 32.0).clamp(280.0, 640.0),
        };
        quick_ask.update(cx, |panel, _| panel.set_width(width));

        let panel = div()
            .id("workspace-quick-ask-panel")
            .debug_selector(|| "workspace-quick-ask-panel".to_string())
            .min_w_0()
            .overflow_hidden()
            .when(
                matches!(position, QuickAskPosition::Top | QuickAskPosition::Bottom),
                |this| this.flex_1().w_full(),
            )
            .when(
                matches!(position, QuickAskPosition::Left | QuickAskPosition::Right),
                |this| this.h_full().w(px(360.0)),
            )
            .when(position == QuickAskPosition::Center, |this| {
                this.w(px(width))
                    .max_h(px((viewport.height.as_f32() - 32.0).max(160.0)))
                    .rounded(cx.theme().radius)
                    .shadow_lg()
            })
            .child(quick_ask);

        match position {
            QuickAskPosition::Top => div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .flex()
                .child(panel),
            QuickAskPosition::Bottom => div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .flex()
                .child(panel),
            QuickAskPosition::Left => div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .flex()
                .child(panel),
            QuickAskPosition::Right => div()
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .flex()
                .child(panel),
            QuickAskPosition::Center => div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .right_0()
                .flex()
                .items_center()
                .justify_center()
                .p(px(16.0))
                .child(panel),
        }
        .into_any_element()
    }

    pub(super) fn ai_context_snapshot(&self, cx: &App) -> WorkspaceAiSnapshot {
        let all = self
            .pane_group
            .panes()
            .into_iter()
            .flat_map(|pane| pane.read(cx).document_views())
            .collect::<Vec<_>>();
        let active_document = self.active_pane.read(cx).active_document_view();
        let current_document = self
            .document_context_mru
            .iter()
            .find_map(|id| {
                all.iter()
                    .find(|document| document.entity_id() == *id)
                    .cloned()
            })
            .or_else(|| active_document.clone())
            .or_else(|| all.first().cloned());

        let mut open_documents = Vec::with_capacity(all.len());
        for id in &self.document_context_mru {
            if let Some(document) = all.iter().find(|document| document.entity_id() == *id) {
                open_documents.push(document.clone());
            }
        }
        if let Some(active_document) = active_document {
            open_documents.push(active_document);
        }
        open_documents.extend(all);

        let mut paths = std::collections::HashSet::new();
        open_documents.retain(|document| paths.insert(normalize_path(document.read(cx).path())));

        WorkspaceAiSnapshot {
            current_document,
            open_documents,
        }
    }

    pub(super) fn ai_context_document(&self, cx: &App) -> Option<Entity<DocumentView>> {
        self.ai_context_snapshot(cx).current_document
    }

    pub(super) fn ai_document_for_path(
        &self,
        path: &Path,
        cx: &App,
    ) -> Option<Entity<DocumentView>> {
        let path = normalize_path(path);
        self.ai_context_snapshot(cx)
            .open_documents
            .into_iter()
            .find(|document| normalize_path(document.read(cx).path()) == path)
    }

    pub(super) fn ai_tool_documents(&self, cx: &App) -> Vec<WorkspaceToolDocument> {
        let snapshot = self.ai_context_snapshot(cx);
        let documents = snapshot.open_documents;
        let current_path = snapshot
            .current_document
            .map(|document| normalize_path(document.read(cx).path()));
        let mut counts = HashMap::<String, usize>::new();
        for document in &documents {
            *counts
                .entry(document.read(cx).file_name().to_string())
                .or_default() += 1;
        }
        let mut ordinals = HashMap::<String, usize>::new();

        documents
            .into_iter()
            .enumerate()
            .filter_map(|(index, document)| {
                let provider = DocumentViewAiContextProvider::new(&document);
                let executor = provider.agent_tools(cx)?;
                let context = provider.context(false, cx);
                let base = document.read(cx).file_name().to_string();
                let ordinal = ordinals.entry(base.clone()).or_default();
                *ordinal += 1;
                let label = if counts.get(&base).copied().unwrap_or(0) > 1 {
                    format!("{base} ({ordinal})")
                } else {
                    base
                };
                let path = normalize_path(document.read(cx).path());
                let definitions = executor.definitions();
                Some(WorkspaceToolDocument {
                    id: format!("doc_{}", index + 1),
                    label,
                    format: context
                        .as_ref()
                        .and_then(|context| context.document_format.clone()),
                    page_count: context.as_ref().map_or(0, |context| context.page_count),
                    current: current_path.as_ref() == Some(&path),
                    can_render: definitions
                        .iter()
                        .any(|definition| definition.name == "render_document_pages"),
                    executor,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct RecordingTools {
        calls: Arc<Mutex<Vec<AgentToolCall>>>,
    }

    impl AgentToolExecutor for RecordingTools {
        fn definitions(&self) -> Vec<AgentToolDefinition> {
            vec![AgentToolDefinition::new(
                "read_document",
                "read",
                json!({"type": "object"}),
            )]
        }

        fn execute(&self, call: &AgentToolCall, _: &AgentCancellationToken) -> AgentToolResult {
            self.calls.lock().unwrap().push(call.clone());
            AgentToolResult::text(&call.id, &call.name, "delegated")
        }
    }

    fn tools(calls: Arc<Mutex<Vec<AgentToolCall>>>) -> WorkspaceAgentTools {
        WorkspaceAgentTools {
            documents: vec![WorkspaceToolDocument {
                id: "doc_1".to_string(),
                label: "paper.pdf".to_string(),
                format: Some("PDF".to_string()),
                page_count: 12,
                current: true,
                executor: Arc::new(RecordingTools { calls }),
                can_render: false,
            }],
        }
    }

    #[test]
    fn workspace_tool_contract_uses_opaque_document_ids() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let tools = tools(calls);
        let names = tools
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "list_open_documents",
                "search_documents",
                "read_document",
                "render_document_pages",
            ]
        );
        let result = tools.execute(
            &AgentToolCall {
                id: "list-1".to_string(),
                name: "list_open_documents".to_string(),
                arguments: "{}".to_string(),
            },
            &AgentCancellationToken::default(),
        );
        let AgentToolResultPart::Text(text) = &result.content[0] else {
            panic!("expected text document list");
        };
        assert!(text.contains("doc_1"));
        assert!(text.contains("paper.pdf"));
        assert!(!text.contains('/'));
    }

    #[test]
    fn read_document_routes_by_id_without_forwarding_workspace_metadata() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let tools = tools(calls.clone());
        let result = tools.execute(
            &AgentToolCall {
                id: "read-1".to_string(),
                name: "read_document".to_string(),
                arguments: r#"{"document_id":"doc_1","pages":[3]}"#.to_string(),
            },
            &AgentCancellationToken::default(),
        );
        assert!(!result.is_error);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_document");
        let arguments = calls[0].arguments_json().unwrap();
        assert!(arguments.get("document_id").is_none());
        assert_eq!(arguments["pages"], json!([3]));
    }
}
