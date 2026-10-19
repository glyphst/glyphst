//! The app's side of the agents' tools: answering [`UiCall`]s between
//! frames, and the document list the reading tools resolve ids with.

use std::path::PathBuf;

use glyphst_ai::tools::ToolRegistry;
use glyphst_core::{DocumentId, DocumentLayout, HighlightColor};
use glyphst_library::{DocumentPosition, LibraryItem, ReadingStatus};
use serde_json::Value;

use super::ReaderApp;
use crate::{
    ai_bridge::{UiCall, UiOutput, UiReply},
    ai_changes::ChangeAction,
    ai_mentions::Sources,
    ai_tools::{OpenDocument, document_id},
    annotations,
    commands::{Category, Command},
    document_tab::DocumentTab,
    document_tab::ViewAction,
};

const MAX_OUTLINE_ENTRIES: usize = 400;
/// Library books the `@` menu offers at most.
const MAX_MENTION_BOOKS: usize = 300;
const MAX_RECENT: usize = 15;

impl ReaderApp {
    /// The tools of an assistant run: the reading tools over the open
    /// documents and the app's tools.
    pub(crate) fn agent_tools(&self) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        self.agent_documents.register(&mut registry);
        self.library_jobs.lookups().register(&mut registry);
        self.agent_bridge.register(&mut registry);
        self.search_index.register(&mut registry);
        registry
    }

    /// The MCP server, started on first use.
    pub(crate) fn mcp_server(&mut self) -> anyhow::Result<&glyphst_agent_host::mcp::McpServer> {
        if self.mcp.is_none() {
            let sessions = glyphst_agent_host::mcp::McpSessions::default();
            self.mcp = Some(glyphst_agent_host::mcp::McpServer::start(sessions)?);
        }
        Ok(self.mcp.as_ref().expect("started above"))
    }

    /// An MCP server config (JSON, as Claude Code, Codex and Pi read it)
    /// for an agent the user starts by hand; its changes show in the
    /// assistant's change log.
    pub(crate) fn mcp_config(&mut self) -> anyhow::Result<String> {
        let tools = std::sync::Arc::new(self.agent_tools());
        let server = self.mcp_server()?;
        let token = server.sessions.open(glyphst_agent_host::mcp::McpSession {
            tools,
            context: glyphst_ai::tools::ToolContext {
                origin: glyphst_ai::tools::ToolOrigin::External("MCP client".into()),
                ..Default::default()
            },
        });
        let config = serde_json::json!({
            "mcpServers": {
                "glyphst": {
                    "type": "http",
                    "url": server.url,
                    "headers": {"Authorization": format!("Bearer {token}")},
                }
            }
        });
        Ok(serde_json::to_string_pretty(&config)?)
    }

    /// What an external agent's turn needs: its program, the document's
    /// working folder, and a new MCP token for the turn (the previous turn's
    /// token of the conversation stops working).
    pub(crate) fn external_access(
        &mut self,
        id: &str,
        ask: &crate::ai::AskContext,
    ) -> anyhow::Result<crate::ai::ExternalAccess> {
        let agent = self
            .agent_specs()
            .into_iter()
            .find(|agent| agent.id() == id)
            .ok_or_else(|| anyhow::anyhow!("the agent {id} is not installed or configured"))?;
        let root = dirs::data_dir()
            .ok_or_else(|| anyhow::anyhow!("the system has no data folder"))?
            .join("glyphst")
            .join("agents");
        let scratch = glyphst_agent_host::scratch::Scratch::open(
            &root,
            &glyphst_agent_host::scratch::Scratch::key(&ask.path),
            &ask.title,
        )?;
        let turn = self.assistant.next_turn();
        let context = self.assistant.tool_context(turn, Some(&agent.name));
        let chat = context.thread.unwrap_or_default();
        let tools = std::sync::Arc::new(self.agent_tools());
        let server = self.mcp_server()?;
        let token = server
            .sessions
            .open(glyphst_agent_host::mcp::McpSession { tools, context });
        let mcp = glyphst_agent_host::external::McpAccess {
            url: server.url.clone(),
            token: token.clone(),
        };
        if let Some(previous) = self.mcp_tokens.insert(chat, token)
            && let Some(server) = &self.mcp
        {
            server.sessions.close(&previous);
        }
        Ok(crate::ai::ExternalAccess {
            agent,
            scratch,
            mcp,
            model: None,
        })
    }

    /// The agents chats can run on: the coding agents found on this
    /// computer and the ACP agents of the settings whose command exists.
    pub(crate) fn agent_specs(&self) -> Vec<glyphst_agent_host::external::AgentSpec> {
        use glyphst_agent_host::external::{AgentKind, AgentSpec};

        let mut agents = self.agent_programs.clone();
        if let Some(settings) = self.user_settings.get() {
            agents.extend(settings.ai.acp_agents.iter().filter_map(|config| {
                let command = std::path::Path::new(&config.command);
                let program = if command.components().count() > 1 {
                    command.is_file().then(|| command.to_path_buf())
                } else {
                    glyphst_agent_host::executable::find(&config.command, None)
                }?;
                Some(AgentSpec {
                    kind: AgentKind::Acp,
                    name: config.name.clone(),
                    program,
                    args: config.args.clone(),
                    env: config.env.clone(),
                })
            }));
        }
        agents
    }

    /// Whether the agents' programs are still being looked for.
    pub(crate) fn agent_search_pending(&self) -> bool {
        self.agent_search.is_some()
    }

    /// Takes the found agents and the files agents changed.
    pub(crate) fn poll_agents(&mut self) {
        if let Some(search) = &self.agent_search
            && let Ok(found) = search.try_recv()
        {
            self.agent_programs = found;
            self.agent_search = None;
        }
        for (context, scratch, changes) in self.assistant.take_file_changes() {
            self.agent_changes.record_files(&context, scratch, changes);
        }
    }

    /// Keeps the reading tools' document list current.
    pub(crate) fn update_agent_documents(&self) {
        let documents = self
            .tabs
            .iter()
            .map(|tab| OpenDocument {
                id: document_id(tab.id),
                title: tab.title.clone(),
                path: tab.path.clone(),
                layout: tab.document_layout,
                page_count: tab.layout.len(),
                current_page: tab.current_page(),
            })
            .collect();
        let active = self.active.map(|active| document_id(self.tabs[active].id));
        self.agent_documents.update(documents, active);
    }

    /// Answers the tool calls waiting for the app.
    pub(crate) fn serve_agent_calls(&mut self, ctx: &egui::Context) {
        while let Ok(call) = self.agent_calls.try_recv() {
            let reply = self.agent_call(&call, ctx);
            if let Ok(output) = &reply {
                if !output.edits.is_empty() {
                    self.agent_changes
                        .record(&call.context, output.edits.clone());
                }
                if !output.vault.is_empty() {
                    self.agent_changes
                        .record_vault(&call.context, output.vault.clone());
                }
            }
            let _ = call.reply.send(reply);
        }
        self.update_agent_documents();
    }

    fn agent_call(&mut self, call: &UiCall, ctx: &egui::Context) -> UiReply {
        let a = &call.arguments;
        let text = |text: String| Ok(UiOutput::from(text));
        match call.tool.as_str() {
            "get_app_state" => text(self.agent_app_state()),
            "list_documents" => text(self.agent_list_documents()),
            "get_document_info" => text(self.agent_document_info(self.agent_tab(a)?)),
            "get_outline" => text(self.agent_outline(self.agent_tab(a)?)),
            "get_selection" => text(self.agent_selection()?),
            "list_annotations" => text(self.agent_annotations(self.agent_tab(a)?)),
            "open_at" => text(self.agent_open_at(a, ctx)?),
            "list_commands" => text(self.agent_commands()),
            "run_command" => text(self.agent_run_command(a, ctx)?),
            "go_to_page" => {
                let index = self.agent_tab(a)?;
                let page = self.agent_page(index, a)?;
                self.activate(Some(index));
                self.pending_actions.push(ViewAction::GoToPage(page));
                ctx.request_repaint();
                text(format!(
                    "Showing page {} of {}.",
                    page + 1,
                    self.tabs[index].title
                ))
            }
            "open_document" => text(self.agent_open(a, ctx)?),
            "add_highlight" => self.agent_add_highlight(a),
            "update_annotation" => self.agent_update_annotation(a),
            "remove_annotation" => self.agent_remove_annotation(a),
            "add_bookmark" => self.agent_add_bookmark(a),
            "set_reading_status" | "set_library_note" => self.agent_edit_item(&call.tool, a),
            other => self
                .agent_library_call(other, a)
                .unwrap_or_else(|| Err(format!("unknown tool: {other}"))),
        }
    }

    /// Undoes, redoes, keeps or shows what agents changed.
    pub(crate) fn apply_change_action(&mut self, action: ChangeAction, ctx: &egui::Context) {
        let failures: Vec<String> = match action {
            ChangeAction::Undo(id) => self
                .agent_changes
                .undo(id, &mut self.library)
                .err()
                .into_iter()
                .collect(),
            ChangeAction::Redo(id) => self
                .agent_changes
                .redo(id, &mut self.library)
                .err()
                .into_iter()
                .collect(),
            ChangeAction::UndoAll => self.agent_changes.undo_where(&mut self.library, |_| true),
            ChangeAction::Keep => {
                self.agent_changes.keep();
                Vec::new()
            }
            ChangeAction::GoTo(id) => {
                match self.agent_changes.get(id).map(|change| &change.target) {
                    Some(crate::ai_changes::ChangeTarget::Library(edit)) => {
                        if let Some(path) = edit.path().map(PathBuf::from) {
                            let page = edit.page();
                            self.open(&path, ctx, None);
                            if let Some(page) = page {
                                self.pending_actions.push(ViewAction::GoToPage(page));
                            }
                        }
                    }
                    Some(crate::ai_changes::ChangeTarget::VaultBlock(edit)) => {
                        ctx.open_url(egui::OpenUrl::new_tab(format!(
                            "file://{}",
                            edit.path.display()
                        )));
                    }
                    Some(crate::ai_changes::ChangeTarget::Files { scratch, .. }) => {
                        ctx.open_url(egui::OpenUrl::new_tab(format!(
                            "file://{}",
                            scratch.dir.display()
                        )));
                    }
                    None => {}
                }
                Vec::new()
            }
        };
        for failure in failures {
            self.notices.push(format!("Could not undo: {failure}"));
        }
        for index in 0..self.tabs.len() {
            self.reload_annotations(index);
        }
    }

    /// Rewinds the assistant's chat to the user message at `index`, undoes
    /// what the agent changed from then on, and puts the message back in
    /// the composer.
    pub(crate) fn restore_assistant(
        &mut self,
        index: usize,
        draft: Option<crate::ai_editor::Draft>,
        ctx: &egui::Context,
    ) {
        let chat = self.assistant.chat_id();
        let failures = self.agent_changes.undo_where(&mut self.library, |change| {
            change.thread == Some(chat) && change.turn >= index
        });
        let rewound = self.assistant.rewind(index);
        match (draft, rewound) {
            // An edited message, or the same one again.
            (Some(draft), _) => self.ask(draft, ctx),
            (None, Some(rewound)) => self.assistant.compose(ctx, rewound),
            (None, None) => {}
        }
        if self.persist {
            self.assistant.save(&mut self.library);
        }
        for failure in failures {
            self.notices.push(format!("Could not undo: {failure}"));
        }
        for index in 0..self.tabs.len() {
            self.reload_annotations(index);
        }
    }

    /// The tab a call names with `document_id`, or the active one.
    pub(super) fn agent_tab(&self, arguments: &Value) -> Result<usize, String> {
        match arguments["document_id"]
            .as_str()
            .filter(|id| !id.is_empty())
        {
            Some(id) => self
                .tabs
                .iter()
                .position(|tab| document_id(tab.id) == id)
                .ok_or_else(|| format!("no open document has the id {id}; call list_documents")),
            None => self.active.ok_or_else(|| "no document is open".to_string()),
        }
    }

    /// The 0-based page a call names with its 1-based `page`.
    fn agent_page(&self, index: usize, arguments: &Value) -> Result<usize, String> {
        let count = self.tabs[index].layout.len();
        match arguments["page"].as_u64() {
            Some(page) if (1..=count as u64).contains(&page) => Ok(page as usize - 1),
            Some(page) => Err(format!("page {page} is not between 1 and {count}")),
            None => Err("page is required".into()),
        }
    }

    fn agent_app_state(&self) -> String {
        let mut lines = vec![format!(
            "Glyphst Reader {} on {}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS
        )];
        if self.tabs.is_empty() {
            lines.push("No documents are open; the home page shows the library.".into());
        }
        for (index, tab) in self.tabs.iter().enumerate() {
            let mut line = format!(
                "- {} \"{}\" ({}, page {} of {})",
                document_id(tab.id),
                tab.title,
                format_of(tab.document_layout),
                tab.current_page() + 1,
                tab.layout.len()
            );
            if Some(index) == self.active {
                line.push_str(", active");
            }
            if tab.translation.is_some() {
                line.push_str(", translated");
            }
            if tab.selection.is_some() {
                line.push_str(", has a selection");
            }
            lines.push(line);
        }
        lines.push(format!("Panes: {}", self.panes.count()));
        lines.push(format!("Theme: {:?}", self.prefs.theme));
        lines.push(format!("Keymap: {:?}", self.prefs.keymap));
        if self.bare() {
            lines.push("Zen mode is on.".into());
        }
        if let Some(settings) = self.user_settings.get() {
            let model = settings.ai.resolved_main();
            lines.push(format!(
                "Assistant model: {} ({})",
                model.model, model.provider_name
            ));
        }
        lines.join("\n")
    }

    fn agent_list_documents(&self) -> String {
        let mut lines = vec!["Open documents:".to_string()];
        for tab in &self.tabs {
            lines.push(format!(
                "- {} \"{}\" ({}, {} pages)",
                document_id(tab.id),
                tab.title,
                format_of(tab.document_layout),
                tab.layout.len()
            ));
        }
        if self.tabs.is_empty() {
            lines.push("- none".into());
        }
        let mut history = self.library.history_entries().to_vec();
        history.sort_by_key(|entry| std::cmp::Reverse(entry.opened));
        let recent = history
            .iter()
            .filter(|entry| !self.tabs.iter().any(|tab| tab.path == entry.path))
            .take(MAX_RECENT)
            .map(
                |entry| match self.library.library_item_for_path(&entry.path) {
                    Some(item) => format!("- {}", describe_item(item)),
                    None => format!(
                        "- \"{}\" (path {})",
                        file_name(&entry.path),
                        entry.path.display()
                    ),
                },
            )
            .collect::<Vec<_>>();
        if !recent.is_empty() {
            lines.push("Recently read:".into());
            lines.extend(recent);
        }
        lines.join("\n")
    }

    fn agent_document_info(&self, index: usize) -> String {
        let tab = &self.tabs[index];
        let page = tab.current_page();
        let mut lines = vec![
            format!("Id: {}", document_id(tab.id)),
            format!("Title: {}", tab.title),
            format!("File: {}", tab.path.display()),
            format!(
                "Format: {}, {} pages",
                format_of(tab.document_layout),
                tab.layout.len()
            ),
            format!("Current page: {}", page + 1),
        ];
        if let Some(entry) = tab.current_outline_entry() {
            lines.push(format!("Current section: {}", tab.outline[entry].title));
        }
        for (label, value) in &tab.metadata {
            lines.push(format!("{label}: {value}"));
        }
        if let Some(item) = self.library.library_item_for_path(&tab.path) {
            lines.push(format!("Library entry: {}", describe_item(item)));
            if !item.abstract_text.is_empty() {
                lines.push(format!("Abstract: {}", item.abstract_text));
            }
            if !item.work_note.is_empty() {
                lines.push(format!("Book note: {}", item.work_note));
            }
            let tags = self
                .library
                .tags_for_item(item.id)
                .into_iter()
                .map(|tag| tag.name.clone())
                .collect::<Vec<_>>();
            if !tags.is_empty() {
                lines.push(format!("Tags: {}", tags.join(", ")));
            }
        }
        lines.push(format!(
            "Annotations: {} highlights and notes, {} bookmarks",
            tab.annotations.items.len(),
            tab.annotations.bookmarks.len()
        ));
        lines.join("\n")
    }

    fn agent_outline(&self, index: usize) -> String {
        let tab = &self.tabs[index];
        if tab.outline.is_empty() {
            return format!("\"{}\" has no table of contents.", tab.title);
        }
        let mut lines = tab
            .outline
            .iter()
            .take(MAX_OUTLINE_ENTRIES)
            .map(|entry| {
                let page = entry
                    .page
                    .map_or_else(String::new, |page| format!(" (page {})", page + 1));
                format!("{}- {}{page}", "  ".repeat(entry.depth), entry.title)
            })
            .collect::<Vec<_>>();
        if tab.outline.len() > MAX_OUTLINE_ENTRIES {
            lines.push(format!(
                "… {} more entries",
                tab.outline.len() - MAX_OUTLINE_ENTRIES
            ));
        }
        lines.join("\n")
    }

    fn agent_selection(&self) -> Result<String, String> {
        let tab = &self.tabs[self.active.ok_or("no document is open")?];
        let selection = tab.selection.as_ref().ok_or("nothing is selected")?;
        let text = tab
            .selected_text()
            .ok_or("the selection's text is still being read")?;
        let pages = selection.pages();
        let pages = if pages.start() == pages.end() {
            format!("page {}", pages.start() + 1)
        } else {
            format!("pages {}-{}", pages.start() + 1, pages.end() + 1)
        };
        Ok(format!("Selected on {pages} of \"{}\":\n{text}", tab.title))
    }

    fn agent_annotations(&self, index: usize) -> String {
        let path = &self.tabs[index].path;
        let page_of = |rects: &[glyphst_library::HighlightRect]| {
            rects
                .first()
                .map_or_else(String::new, |rect| format!("page {}, ", rect.page_idx + 1))
        };
        let mut lines = Vec::new();
        for highlight in self.library.highlights_for(path) {
            lines.push(format!(
                "- h{} highlight ({}{:?}): \"{}\"",
                highlight.id,
                page_of(&highlight.rects),
                highlight.color,
                highlight.text
            ));
        }
        for note in self.library.notes_for(path) {
            lines.push(format!(
                "- n{} note ({}{:?}) on \"{}\": {}",
                note.id,
                page_of(&note.rects),
                note.color,
                note.text,
                note.body
            ));
        }
        for bookmark in self.library.bookmarks_for(path) {
            lines.push(format!(
                "- b{} bookmark (page {}){}",
                bookmark.id,
                bookmark.position.page,
                bookmark
                    .title
                    .map_or_else(String::new, |title| format!(": {title}"))
            ));
        }
        if lines.is_empty() {
            "The document has no highlights, notes or bookmarks.".into()
        } else {
            lines.join("\n")
        }
    }

    fn agent_commands(&self) -> String {
        Command::ALL
            .into_iter()
            .map(|command| {
                let mut line = format!(
                    "- {} ({}): {}",
                    command.id(),
                    command.category().label(),
                    command.label()
                );
                if !agent_may_run(command) {
                    line.push_str(" [not for agents]");
                } else if let Some(reason) = self.command_blocked(command) {
                    line.push_str(&format!(" [unavailable: {reason}]"));
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn agent_run_command(
        &mut self,
        arguments: &Value,
        ctx: &egui::Context,
    ) -> Result<String, String> {
        let id = arguments["id"].as_str().unwrap_or_default();
        let command = Command::from_id(id)
            .ok_or_else(|| format!("unknown command {id}; call list_commands"))?;
        if !agent_may_run(command) {
            return Err(format!("{id} is not available to agents"));
        }
        self.run(command, ctx).map_err(str::to_string)?;
        ctx.request_repaint();
        Ok(format!("Ran {} ({}).", command.id(), command.label()))
    }

    fn agent_open(&mut self, arguments: &Value, ctx: &egui::Context) -> Result<String, String> {
        let path = match (arguments["library_id"].as_str(), arguments["path"].as_str()) {
            (Some(id), _) => {
                let id = parse_library_id(id)?;
                self.library
                    .primary_attachment(id)
                    .map(|attachment| attachment.path.clone())
                    .ok_or_else(|| "the book has no file".to_string())?
            }
            (None, Some(path)) => PathBuf::from(path),
            (None, None) => return Err("give a library_id or a path".into()),
        };
        if !path.exists() {
            return Err(format!("{} does not exist", path.display()));
        }
        self.open(&path, ctx, None);
        let tab = self
            .tabs
            .iter()
            .find(|tab| tab.path == path)
            .ok_or_else(|| format!("could not open {}", path.display()))?;
        Ok(format!(
            "Opened \"{}\" as {} ({} pages).",
            tab.title,
            document_id(tab.id),
            tab.layout.len()
        ))
    }

    fn agent_add_highlight(&mut self, arguments: &Value) -> UiReply {
        let index = self.agent_tab(arguments)?;
        let page = self.agent_page(index, arguments)?;
        let quote = arguments["quote"]
            .as_str()
            .map(str::trim)
            .filter(|quote| !quote.is_empty())
            .ok_or("quote must not be empty")?;
        let occurrence = arguments["occurrence"].as_u64().unwrap_or(1).max(1) as usize;
        let color = parse_color(&arguments["color"])?.unwrap_or(HighlightColor::Yellow);
        let note = arguments["note"]
            .as_str()
            .map(str::trim)
            .filter(|note| !note.is_empty());
        let tab = &self.tabs[index];
        let annotations::SelectionRecord {
            rects,
            quote: quoted,
            semantic,
        } = annotations::quote_record(&tab.text, page, quote, occurrence, tab.is_reflowed())?;
        let path = tab.path.clone();
        let (created, edits) = self.library.record_edits(|library| match (semantic, note) {
            (Some(range), None) => library
                .add_reflow_highlight(&path, color, range, rects, quoted.clone(), "")
                .map(|highlight| format!("h{}", highlight.id)),
            (None, None) => library
                .add_highlight(&path, color, rects, quoted.clone(), "")
                .map(|highlight| format!("h{}", highlight.id)),
            (Some(range), Some(note)) => library
                .add_reflow_note(&path, color, range, rects, quoted.clone(), note)
                .map(|note| format!("n{}", note.id)),
            (None, Some(note)) => library
                .add_note(&path, color, rects, quoted.clone(), note)
                .map(|note| format!("n{}", note.id)),
        });
        let id = created.map_err(|error| format!("could not save: {error:#}"))?;
        self.reload_annotations(index);
        Ok(UiOutput {
            text: format!("Added {id} on page {}: \"{quoted}\".", page + 1),
            edits,
            ..UiOutput::default()
        })
    }

    fn agent_update_annotation(&mut self, arguments: &Value) -> UiReply {
        let index = self.agent_tab(arguments)?;
        let (kind, id) = parse_annotation_id(arguments)?;
        let color = parse_color(&arguments["color"])?;
        let body = arguments["note"].as_str();
        let path = self.tabs[index].path.clone();
        let (result, edits) = self.library.record_edits(|library| match kind {
            'h' => {
                let color = color.ok_or_else(|| anyhow::anyhow!("give a color for a highlight"))?;
                library.set_highlight_color(id, color)
            }
            'n' => {
                let note = library
                    .notes_for(&path)
                    .into_iter()
                    .find(|note| note.id == id)
                    .ok_or_else(|| anyhow::anyhow!("the document has no note n{id}"))?;
                library.update_note(
                    id,
                    body.map_or(note.body, str::to_string),
                    color.unwrap_or(note.color),
                )
            }
            _ => Err(anyhow::anyhow!(
                "bookmarks cannot be changed; remove and add them"
            )),
        });
        self.agent_edit_result(index, result, edits, format!("Changed {kind}{id}."))
    }

    fn agent_remove_annotation(&mut self, arguments: &Value) -> UiReply {
        let index = self.agent_tab(arguments)?;
        let (kind, id) = parse_annotation_id(arguments)?;
        let (result, edits) = self.library.record_edits(|library| match kind {
            'h' => library.remove_highlight(id),
            'n' => library.remove_note(id),
            _ => library.remove_bookmark(id),
        });
        self.agent_edit_result(index, result, edits, format!("Removed {kind}{id}."))
    }

    fn agent_add_bookmark(&mut self, arguments: &Value) -> UiReply {
        let index = self.agent_tab(arguments)?;
        let page = self.agent_page(index, arguments)?;
        let tab = &self.tabs[index];
        let position =
            DocumentPosition::fixed(0.0, tab.layout.offsets[page][1] as f32, 1.0, page + 1);
        let title = arguments["title"]
            .as_str()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_string);
        let path = tab.path.clone();
        let (result, edits) = self
            .library
            .record_edits(|library| library.add_bookmark(&path, title, position));
        let id = result.map_err(|error| format!("could not save: {error:#}"))?;
        self.reload_annotations(index);
        Ok(UiOutput {
            text: format!("Added bookmark b{id} on page {}.", page + 1),
            edits,
            ..UiOutput::default()
        })
    }

    fn agent_edit_item(&mut self, tool: &str, arguments: &Value) -> UiReply {
        let id = match arguments["library_id"].as_str() {
            Some(id) => parse_library_id(id)?,
            None => {
                let index = self.agent_tab(arguments)?;
                self.library
                    .library_item_for_path(&self.tabs[index].path)
                    .map(|item| item.id)
                    .ok_or("the document is not in the library")?
            }
        };
        let (result, edits) = if tool == "set_reading_status" {
            let status = match arguments["status"].as_str() {
                Some("to_read") => ReadingStatus::ToRead,
                Some("reading") => ReadingStatus::Reading,
                Some("read") => ReadingStatus::Read,
                _ => return Err("status must be to_read, reading or read".into()),
            };
            self.library
                .record_edits(|library| library.set_library_item_status(id, status))
        } else {
            let note = arguments["note"].as_str().unwrap_or_default().to_string();
            self.library
                .record_edits(|library| library.set_library_item_note(id, note))
        };
        result.map_err(|error| format!("could not save: {error:#}"))?;
        let title = self
            .library
            .library_item(id)
            .map(|item| item.title.clone())
            .unwrap_or_default();
        Ok(UiOutput {
            text: format!("Updated \"{title}\"."),
            edits,
            ..UiOutput::default()
        })
    }

    fn agent_edit_result(
        &mut self,
        index: usize,
        result: anyhow::Result<bool>,
        edits: Vec<glyphst_library::LibraryEdit>,
        done: String,
    ) -> UiReply {
        match result {
            Ok(true) => {
                self.reload_annotations(index);
                Ok(UiOutput {
                    text: done,
                    edits,
                    ..UiOutput::default()
                })
            }
            Ok(false) => Err("no annotation has that id; call list_annotations".into()),
            Err(error) => Err(format!("{error:#}")),
        }
    }
}

/// Looks for the coding agents' programs on a worker thread (an
/// environment variable such as `CLAUDE_CODE_EXECUTABLE` can name one).
pub(crate) fn find_agents(
    ctx: &egui::Context,
) -> std::sync::mpsc::Receiver<Vec<glyphst_agent_host::external::AgentSpec>> {
    use glyphst_agent_host::{
        executable,
        external::{AgentKind, AgentSpec},
    };

    let (sender, receiver) = std::sync::mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let found = AgentKind::KNOWN
            .into_iter()
            .filter_map(|kind| {
                let configured = kind
                    .program_variable()
                    .and_then(std::env::var_os)
                    .map(PathBuf::from);
                executable::find(kind.program(), configured.as_deref())
                    .map(|program| AgentSpec::known(kind, program))
            })
            .collect();
        let _ = sender.send(found);
        ctx.request_repaint();
    });
    receiver
}

/// What the assistant's `@` menu can offer: the open documents (the active
/// one first), its outline, selection and annotations, and library books.
pub(crate) fn mention_sources(
    tabs: &[DocumentTab],
    active: Option<usize>,
    library: &glyphst_library::LibraryStore,
) -> Sources {
    let open = |tab: &DocumentTab| OpenDocument {
        id: document_id(tab.id),
        title: tab.title.clone(),
        path: tab.path.clone(),
        layout: tab.document_layout,
        page_count: tab.layout.len(),
        current_page: tab.current_page(),
    };
    let mut sources = Sources::default();
    if let Some(tab) = active.map(|active| &tabs[active]) {
        sources.documents.push(open(tab));
        sources.outline = tab.outline.clone();
        sources.selection = tab.selected_text().and_then(|text| {
            let page = *tab.selection.as_ref()?.pages().start();
            Some((page, text))
        });
        for highlight in library.highlights_for(&tab.path) {
            let page = highlight.rects.first().map_or(0, |rect| rect.page_idx + 1);
            sources.annotations.push((
                format!("Highlight p. {page}: {}", highlight.text),
                format!("Highlighted on page {page}: \"{}\"", highlight.text),
            ));
        }
        for note in library.notes_for(&tab.path) {
            let page = note.rects.first().map_or(0, |rect| rect.page_idx + 1);
            sources.annotations.push((
                format!("Note p. {page}: {}", note.body),
                format!("Note on page {page} about \"{}\": {}", note.text, note.body),
            ));
        }
    }
    sources.documents.extend(
        tabs.iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != active)
            .map(|(_, tab)| open(tab)),
    );
    let mut items = library
        .library_items()
        .iter()
        .filter(|item| item.trashed_at.is_none())
        .collect::<Vec<_>>();
    items.sort_by_key(|item| std::cmp::Reverse(item.updated));
    sources.books = items
        .into_iter()
        .take(MAX_MENTION_BOOKS)
        .map(|item| {
            let mut details = describe_item(item);
            if !item.abstract_text.is_empty() {
                details.push_str(&format!("\nAbstract: {}", item.abstract_text));
            }
            if !item.work_note.is_empty() {
                details.push_str(&format!("\nNote: {}", item.work_note));
            }
            details.push_str("\nOpen it with open_document to read it.");
            let label = match item.creators.first() {
                Some(creator) => format!("{} by {}", item.title, creator.name),
                None => item.title.clone(),
            };
            (label, details)
        })
        .collect();
    sources
}

/// Commands agents may run: moving around and changing the view, never
/// opening dialogs, closing documents or translating (which costs).
fn agent_may_run(command: Command) -> bool {
    match command.category() {
        Category::Navigation => true,
        Category::View => !matches!(
            command,
            Command::ClosePane | Command::Reload | Command::ToggleFullscreen
        ),
        Category::Text => matches!(
            command,
            Command::Find | Command::FindNext | Command::FindPrevious
        ),
        Category::App => matches!(
            command,
            Command::ThemeSystem
                | Command::ThemeLight
                | Command::ThemeDark
                | Command::ToggleSidebar
                | Command::NextTab
                | Command::PreviousTab
        ),
        Category::Translation => command == Command::ShowTranslated,
        Category::Assistant | Category::Present => false,
    }
}

fn format_of(layout: DocumentLayout) -> &'static str {
    match layout {
        DocumentLayout::Fixed => "PDF",
        DocumentLayout::Reflow(_) => "EPUB",
    }
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub(super) fn describe_item(item: &LibraryItem) -> String {
    let mut text = format!("lib_{} \"{}\"", item.id.0, item.title);
    let creators = item
        .creators
        .iter()
        .map(|creator| creator.name.as_str())
        .collect::<Vec<_>>();
    if !creators.is_empty() {
        text.push_str(&format!(" by {}", creators.join(", ")));
    }
    if let Some(year) = item.year {
        text.push_str(&format!(" ({year})"));
    }
    text.push_str(match item.status {
        ReadingStatus::ToRead => ", to read",
        ReadingStatus::Reading => ", reading",
        ReadingStatus::Read => ", read",
    });
    text
}

pub(super) fn parse_library_id(id: &str) -> Result<DocumentId, String> {
    id.trim_start_matches("lib_")
        .parse()
        .map(DocumentId)
        .map_err(|_| format!("{id} is not a library id like lib_12"))
}

fn parse_annotation_id(arguments: &Value) -> Result<(char, u64), String> {
    let id = arguments["annotation_id"]
        .as_str()
        .unwrap_or_default()
        .trim();
    let mut chars = id.chars();
    let kind = chars.next().filter(|kind| matches!(kind, 'h' | 'n' | 'b'));
    match (kind, chars.as_str().parse()) {
        (Some(kind), Ok(number)) => Ok((kind, number)),
        _ => Err(format!(
            "{id} is not an annotation id like h3, n5 or b2; call list_annotations"
        )),
    }
}

fn parse_color(value: &Value) -> Result<Option<HighlightColor>, String> {
    let Some(name) = value.as_str() else {
        return Ok(None);
    };
    Ok(Some(match name {
        "yellow" => HighlightColor::Yellow,
        "green" => HighlightColor::Green,
        "blue" => HighlightColor::Blue,
        "pink" => HighlightColor::Pink,
        "orange" => HighlightColor::Orange,
        other => return Err(format!("{other} is not a highlight color")),
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn tool_arguments_parse_and_agents_run_only_safe_commands() {
        assert_eq!(
            parse_annotation_id(&json!({"annotation_id": "n12"})),
            Ok(('n', 12))
        );
        assert!(parse_annotation_id(&json!({"annotation_id": "x1"})).is_err());
        assert_eq!(parse_library_id("lib_7"), Ok(DocumentId(7)));
        assert!(parse_library_id("doc_7").is_err());
        assert_eq!(parse_color(&json!("pink")), Ok(Some(HighlightColor::Pink)));
        assert_eq!(parse_color(&Value::Null), Ok(None));
        assert!(parse_color(&json!("red")).is_err());
        assert!(agent_may_run(Command::NextPage));
        assert!(agent_may_run(Command::ZoomIn));
        assert!(!agent_may_run(Command::Open));
        assert!(!agent_may_run(Command::OpenWithDialog));
        assert!(!agent_may_run(Command::CloseTab));
        assert!(!agent_may_run(Command::TranslateDocument));
        assert!(!agent_may_run(Command::QuickAsk));
    }
}
