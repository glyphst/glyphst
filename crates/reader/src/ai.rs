//! The assistant: a chat about the open document in a right-side panel.
//!
//! Turns run the `ai` crate's agent on a worker thread with the document
//! tools of [`OpenDocument`]; the current page and selection can be
//! attached as context, and each message ends with what the reader showed. Sessions are the library's AI sessions, shared with
//! the GPUI Reader's AI panel. The panel follows pdfcraft's side panels;
//! answers are Markdown.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    thread,
};

use glyphst_ai::{
    agent::{
        AgentCancellationToken, AgentContentPart, AgentEvent, AgentLimits, AgentMessage, AgentRun,
        AgentStopReason, AgentToolResultPart, run_agent,
    },
    ai_chat::{ChatMessage, ContextBlock, DocumentContextScope, ImageContext, MentionKind, Role},
    prompt::{DocumentFacts, ReaderState},
    tools::{ToolContext, ToolRegistry},
};
use glyphst_core::DocumentLayout;
use glyphst_library::{
    AiSession, AiSessionAgentPart, AiSessionMessage, AiSessionRole, AiSessionToolResultPart,
    AiSessionToolStatus, LibraryStore, current_timestamp,
};
use glyphst_settings::{AiProvider, ModelRef, Settings};

use crate::{
    ai_tools::{OpenDocument, describe_call},
    theme::Tokens,
};

const TITLE_CHARS: usize = 60;

/// What a turn knows about the open document.
pub struct AskContext {
    pub path: PathBuf,
    pub title: String,
    pub layout: DocumentLayout,
    pub page_count: usize,
    /// The page shown, from 0.
    pub current_page: usize,
    pub page_text: Option<String>,
    pub selection: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToolState {
    Running,
    Done,
    Failed,
}

/// A tool call shown in an answer.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolRow {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub state: ToolState,
}

impl ToolRow {
    pub fn label(&self) -> String {
        describe_call(&self.name, &self.arguments)
    }
}

enum RunEvent {
    Agent(AgentEvent),
    Finished(Vec<AgentMessage>),
    Error(String),
}

struct Run {
    cancel: AgentCancellationToken,
    events: Receiver<RunEvent>,
    /// The answer being written.
    answer: usize,
}

pub struct Assistant {
    pub open: bool,
    session_id: u64,
    created: u64,
    messages: Vec<ChatMessage>,
    /// Tool calls of each message.
    tools: Vec<Vec<ToolRow>>,
    pub composer: String,
    pub include_page: bool,
    pub include_selection: bool,
    run: Option<Run>,
    pub error: Option<String>,
    markdown: egui_commonmark::CommonMarkCache,
    show_sessions: bool,
    sessions: Vec<AiSession>,
    /// Images the next message carries (page regions).
    pub attachments: Vec<Attachment>,
    /// Thumbnails of the images sent with each message, by index.
    sent_images: std::collections::HashMap<usize, Vec<egui::TextureHandle>>,
    /// The model's reasoning summary for each answer, by index.
    reasoning: std::collections::HashMap<usize, String>,
    /// What the running turn waits for, such as a retry.
    status: Option<String>,
    /// Names the conversation to the provider's prompt cache.
    cache_key: Option<String>,
}

/// An image attached to the next message.
pub struct Attachment {
    pub image: ImageContext,
    pub thumbnail: egui::TextureHandle,
    pub label: String,
}

impl Default for Assistant {
    fn default() -> Self {
        Self {
            open: false,
            session_id: 0,
            created: 0,
            messages: Vec::new(),
            tools: Vec::new(),
            composer: String::new(),
            include_page: true,
            include_selection: true,
            run: None,
            error: None,
            markdown: egui_commonmark::CommonMarkCache::default(),
            attachments: Vec::new(),
            sent_images: Default::default(),
            reasoning: Default::default(),
            status: None,
            cache_key: None,
            show_sessions: false,
            sessions: Vec::new(),
        }
    }
}

/// What the panel asks for.
#[derive(Debug, PartialEq)]
pub enum PanelAction {
    Send(String),
    NewSession,
    LoadSession(u64),
    /// The user chose the assistant's model.
    SetModel(ModelRef),
}

impl Assistant {
    pub fn is_running(&self) -> bool {
        self.run.is_some()
    }

    #[cfg(test)]
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    #[cfg(test)]
    pub fn tools(&self, message: usize) -> &[ToolRow] {
        self.tools.get(message).map_or(&[], Vec::as_slice)
    }

    pub fn refresh_sessions(&mut self, library: &LibraryStore) {
        let mut sessions = library.ai_sessions();
        sessions.sort_by_key(|session| std::cmp::Reverse(session.updated));
        self.sessions = sessions;
    }

    /// Starts a turn: the message, with the page and selection as context
    /// when chosen, then the agent with the document tools.
    pub fn send(
        &mut self,
        text: String,
        ask: AskContext,
        settings: &Settings,
        ctx: &egui::Context,
    ) {
        let text = text.trim().to_string();
        if text.is_empty() || self.run.is_some() {
            return;
        }
        self.error = None;
        let page = ask.current_page + 1;
        let mut contexts = Vec::new();
        if self.include_page
            && let Some(page_text) = &ask.page_text
        {
            contexts.push(ContextBlock::new(
                MentionKind::Page,
                Some(page),
                page_text.clone(),
            ));
        }
        if self.include_selection
            && let Some(selection) = &ask.selection
        {
            contexts.push(ContextBlock::new(
                MentionKind::Selection,
                Some(page),
                selection.clone(),
            ));
        }
        let scope = DocumentContextScope {
            path: ask.path.clone(),
            label: ask.title.clone(),
        };
        let state = ReaderState {
            document: Some(DocumentFacts {
                id: None,
                title: ask.title.clone(),
                format: match ask.layout {
                    DocumentLayout::Fixed => "PDF",
                    DocumentLayout::Reflow(_) => "EPUB",
                }
                .into(),
                authors: None,
                page_count: ask.page_count,
            }),
            current_page: Some(page),
            selection_chars: ask.selection.as_ref().map(|text| text.chars().count()),
            ..ReaderState::default()
        };
        let mut message = ChatMessage::user(text.clone(), text, contexts, Vec::new())
            .with_document_scope(Some(scope))
            .with_reader_state(state.block());
        let mut thumbnails = Vec::new();
        for attachment in std::mem::take(&mut self.attachments) {
            message.attach_image(attachment.image);
            thumbnails.push(attachment.thumbnail);
        }
        if !thumbnails.is_empty() {
            self.sent_images.insert(self.messages.len(), thumbnails);
        }
        self.messages.push(message);
        self.tools.push(Vec::new());

        let config = settings.ai.resolved_main();
        if !config.can_request() {
            self.error = Some(if config.provider == AiProvider::ChatGptSubscription {
                "Sign in to ChatGPT in Settings > AI, or choose an API-key model there.".into()
            } else {
                format!(
                    "{}. Set up a model in Settings > AI.",
                    config.missing_credentials_message()
                )
            });
            return;
        }
        let history: Vec<AgentMessage> = self
            .messages
            .iter()
            .flat_map(ChatMessage::into_agent_messages)
            .collect();
        self.messages.push(ChatMessage::assistant(String::new()));
        self.tools.push(Vec::new());
        let mut tools = ToolRegistry::new();
        OpenDocument {
            path: ask.path,
            layout: ask.layout,
            page_count: ask.page_count,
            current_page: ask.current_page,
        }
        .register(&mut tools);
        let cache_key = self
            .cache_key
            .get_or_insert_with(|| format!("glyphst-chat-{}", unique_suffix()))
            .clone();
        let cancel = AgentCancellationToken::default();
        let (events_tx, events) = mpsc::channel();
        let (worker_cancel, ctx) = (cancel.clone(), ctx.clone());
        thread::spawn(move || {
            let mut send = |event| {
                ctx.request_repaint();
                events_tx
                    .send(RunEvent::Agent(event))
                    .map_err(|_| anyhow::anyhow!("closed"))
            };
            let result = run_agent(
                AgentRun {
                    settings: config,
                    messages: history,
                    tools: Some(Arc::new(tools)),
                    context: ToolContext {
                        cancel: worker_cancel,
                        ..ToolContext::default()
                    },
                    limits: AgentLimits::default(),
                    cache_key: Some(cache_key),
                },
                &mut send,
            );
            let _ = events_tx.send(match result {
                Ok(result) => RunEvent::Finished(result.appended_messages),
                Err(error) => RunEvent::Error(format!("{error:#}")),
            });
            ctx.request_repaint();
        });
        self.run = Some(Run {
            cancel,
            events,
            answer: self.messages.len() - 1,
        });
    }

    /// Applies the running turn's progress; saves the session when it ends.
    pub fn poll(&mut self, library: Option<&mut LibraryStore>) {
        let Some(run) = &self.run else {
            return;
        };
        let answer = run.answer;
        let events: Vec<RunEvent> = run.events.try_iter().collect();
        let mut finished = false;
        for event in events {
            match event {
                RunEvent::Agent(event) => self.apply(answer, event),
                RunEvent::Finished(trace) => {
                    self.messages[answer].set_agent_trace(trace);
                    finished = true;
                }
                RunEvent::Error(error) => {
                    if error != "agent run canceled" {
                        self.error = Some(error);
                    }
                    finished = true;
                }
            }
        }
        if finished {
            self.run = None;
            self.status = None;
            for row in &mut self.tools[answer] {
                if row.state == ToolState::Running {
                    row.state = ToolState::Failed;
                }
            }
            if let Some(library) = library {
                self.save(library);
            }
        }
    }

    fn apply(&mut self, answer: usize, event: AgentEvent) {
        let rows = &mut self.tools[answer];
        if !matches!(event, AgentEvent::Retrying { .. }) {
            self.status = None;
        }
        match event {
            AgentEvent::TextDelta(delta) => self.messages[answer].push_text(&delta),
            AgentEvent::ReasoningDelta(delta) => {
                self.reasoning.entry(answer).or_default().push_str(&delta);
            }
            AgentEvent::Retrying { attempt, delay, .. } => {
                self.status = Some(format!(
                    "The provider is busy; trying again in {:.0} s (attempt {attempt})…",
                    delay.as_secs_f32().ceil()
                ));
            }
            AgentEvent::TurnCompleted {
                stop_reason: AgentStopReason::Refusal,
                ..
            } => {
                self.error = Some("The model declined this request.".into());
            }
            AgentEvent::ToolCallStarted { id, name } => rows.push(ToolRow {
                id,
                name,
                arguments: String::new(),
                state: ToolState::Running,
            }),
            AgentEvent::ToolCallArgumentsDelta { id, delta } => {
                if let Some(row) = rows.iter_mut().find(|row| row.id == id) {
                    row.arguments.push_str(&delta);
                }
            }
            AgentEvent::ToolCallCompleted(call) => {
                if let Some(row) = rows.iter_mut().find(|row| row.id == call.id) {
                    row.arguments = call.arguments;
                }
            }
            AgentEvent::ToolExecutionCompleted(result) => {
                if let Some(row) = rows.iter_mut().find(|row| row.id == result.call_id) {
                    row.state = if result.is_error {
                        ToolState::Failed
                    } else {
                        ToolState::Done
                    };
                }
            }
            AgentEvent::LimitReached => {
                self.error =
                    Some("The assistant stopped after many tool calls; ask it to continue.".into());
            }
            AgentEvent::TurnStarted { .. }
            | AgentEvent::ToolExecutionStarted { .. }
            | AgentEvent::TurnCompleted { .. }
            | AgentEvent::Done => {}
        }
    }

    pub fn stop(&self) {
        if let Some(run) = &self.run {
            run.cancel.cancel();
        }
    }

    pub fn new_session(&mut self) {
        self.stop();
        self.run = None;
        self.session_id = 0;
        self.created = 0;
        self.messages.clear();
        self.tools.clear();
        self.sent_images.clear();
        self.reasoning.clear();
        self.status = None;
        self.cache_key = None;
        self.error = None;
        self.show_sessions = false;
    }

    /// The last answer's text, while one is written or once done.
    pub fn latest_answer(&self) -> Option<String> {
        self.messages
            .last()
            .filter(|message| message.role == Role::Assistant)
            .map(ChatMessage::display_text)
    }

    /// Takes `other`'s conversation as a new chat (a quick question
    /// continued here); false while either is answering.
    pub fn adopt(&mut self, other: &mut Assistant) -> bool {
        if self.run.is_some() || other.run.is_some() {
            return false;
        }
        self.new_session();
        self.messages = std::mem::take(&mut other.messages);
        self.tools = std::mem::take(&mut other.tools);
        self.sent_images = std::mem::take(&mut other.sent_images);
        self.reasoning = std::mem::take(&mut other.reasoning);
        self.cache_key = other.cache_key.take();
        other.new_session();
        true
    }

    pub fn load_session(&mut self, id: u64) {
        let Some(session) = self
            .sessions
            .iter()
            .find(|session| session.id == id)
            .cloned()
        else {
            return;
        };
        self.new_session();
        self.session_id = session.id;
        self.created = session.created;
        for message in session.messages {
            self.tools.push(tool_rows(&message.agent_parts));
            self.messages.push(chat_message(message));
        }
    }

    /// Saves the conversation as a library AI session.
    pub fn save(&mut self, library: &mut LibraryStore) {
        if self.messages.iter().all(ChatMessage::is_display_empty) {
            return;
        }
        let now = current_timestamp();
        if self.session_id == 0 {
            self.session_id = library.next_ai_session_id();
            self.created = now;
        }
        let title = self
            .messages
            .iter()
            .find(|message| message.role == Role::User)
            .map(|message| {
                let text = message.display_text();
                let short: String = text.chars().take(TITLE_CHARS).collect();
                if text.chars().count() > TITLE_CHARS {
                    format!("{short}…")
                } else {
                    short
                }
            })
            .unwrap_or_else(|| "Chat".into());
        let session = AiSession {
            id: self.session_id,
            title,
            messages: self.messages.iter().map(session_message).collect(),
            queued_messages: Vec::new(),
            created: self.created,
            updated: now,
        };
        if let Err(error) = library.save_ai_session(session) {
            self.error = Some(format!("Could not save the chat: {error:#}"));
        }
        self.refresh_sessions(library);
    }

    /// The panel. `can_attach` says whether the page and selection exist.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        has_page: bool,
        has_selection: bool,
        models: &[(ModelRef, String)],
        model: Option<ModelRef>,
    ) -> Option<PanelAction> {
        let tokens = Tokens::get(ui.ctx());
        let mut action = None;
        ui.horizontal(|ui| {
            ui.add(crate::icons::image("sparkles", 16.0, tokens.accent));
            ui.label(egui::RichText::new("Assistant").font(crate::theme::semibold(15.0)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if crate::icons::button(ui, "message-square-plus", 28.0, false, "New chat")
                    .clicked()
                {
                    action = Some(PanelAction::NewSession);
                }
                if crate::icons::button(ui, "clock", 28.0, self.show_sessions, "Saved chats")
                    .clicked()
                {
                    self.show_sessions = !self.show_sessions;
                }
            });
        });
        if !models.is_empty() {
            let mut chosen = model;
            if crate::settings_providers::model_picker(
                ui,
                "assistant-model",
                &mut chosen,
                models,
                None,
            ) && let Some(chosen) = chosen
            {
                action = Some(PanelAction::SetModel(chosen));
            }
        }
        ui.add_space(4.0);
        if self.show_sessions {
            crate::widgets::section_title(ui, "Saved chats");
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    if self.sessions.is_empty() {
                        ui.label(egui::RichText::new("No saved chats.").color(tokens.text_muted));
                    }
                    for session in &self.sessions {
                        let selected = session.id == self.session_id;
                        let (rect, response) = crate::widgets::list_row(ui, 30.0, selected);
                        crate::widgets::row_text(
                            ui,
                            rect.left_center() + egui::vec2(8.0, 0.0),
                            session.title.as_str(),
                            crate::theme::regular(13.0),
                            if selected {
                                tokens.accent_text
                            } else {
                                tokens.text
                            },
                            rect.width() - 16.0,
                        );
                        if response.clicked() {
                            action = Some(PanelAction::LoadSession(session.id));
                        }
                    }
                });
            return action;
        }

        let composer_height = 150.0;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .max_height((ui.available_height() - composer_height).max(80.0))
            .show(ui, |ui| {
                if self.messages.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "Ask about this document. The assistant can search it, read pages and look at them.",
                        )
                        .color(tokens.text_muted),
                    );
                }
                for (index, message) in self.messages.iter().enumerate() {
                    match message.role {
                        Role::User => {
                            let width = ui.available_width();
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                                egui::Frame::new()
                                    .fill(tokens.accent_soft)
                                    .corner_radius(egui::CornerRadius {
                                        nw: 12,
                                        ne: 12,
                                        sw: 12,
                                        se: 4,
                                    })
                                    .inner_margin(egui::Margin::symmetric(12, 8))
                                    .show(ui, |ui| {
                                        ui.set_max_width(width * 0.85);
                                        ui.vertical(|ui| {
                                            for thumbnail in
                                                self.sent_images.get(&index).into_iter().flatten()
                                            {
                                                ui.add(
                                                    egui::Image::new(thumbnail)
                                                        .max_size(egui::vec2(width * 0.6, 160.0))
                                                        .corner_radius(6),
                                                );
                                            }
                                            ui.add(egui::Label::new(message.display_text()).wrap());
                                        });
                                    });
                            });
                        }
                        Role::Assistant => {
                            if let Some(reasoning) = self.reasoning.get(&index) {
                                egui::CollapsingHeader::new(
                                    egui::RichText::new("Thoughts").small().color(tokens.text_faint),
                                )
                                .id_salt(("assistant-reasoning", index))
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(reasoning.trim())
                                            .small()
                                            .color(tokens.text_muted),
                                    );
                                });
                            }
                            for row in self.tools.get(index).into_iter().flatten() {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    match row.state {
                                        ToolState::Running => {
                                            ui.add(egui::Spinner::new().size(12.0));
                                        }
                                        ToolState::Done => {
                                            ui.add(crate::icons::image("check", 13.0, tokens.text_faint));
                                        }
                                        ToolState::Failed => {
                                            ui.add(crate::icons::image("x", 13.0, tokens.danger));
                                        }
                                    }
                                    ui.label(
                                        egui::RichText::new(row.label())
                                            .small()
                                            .color(tokens.text_faint),
                                    );
                                });
                            }
                            let text = message.display_text();
                            if text.is_empty() && self.run.as_ref().is_some_and(|run| run.answer == index) {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label(egui::RichText::new("Thinking…").color(tokens.text_muted));
                                });
                            } else {
                                egui_commonmark::CommonMarkViewer::new().show(
                                    ui,
                                    &mut self.markdown,
                                    &text,
                                );
                            }
                        }
                    }
                    ui.add_space(8.0);
                }
                if let Some(status) = &self.status {
                    ui.label(egui::RichText::new(status).color(tokens.text_muted));
                }
                if let Some(error) = &self.error {
                    ui.label(egui::RichText::new(error).color(tokens.danger));
                }
            });
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(tokens.field)
            .stroke(egui::Stroke::new(1.0, tokens.border))
            .corner_radius(12)
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                attachment_chips(ui, &mut self.attachments, &tokens);
                let input = ui.add(
                    egui::TextEdit::multiline(&mut self.composer)
                        .id(egui::Id::new("assistant-input"))
                        .hint_text("Ask about this document")
                        .frame(egui::Frame::NONE)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                let enter = input.has_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(has_page, |ui| {
                        ui.checkbox(&mut self.include_page, "This page")
                    });
                    ui.add_enabled_ui(has_selection, |ui| {
                        ui.checkbox(&mut self.include_selection, "Selection")
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.run.is_some() {
                            if crate::icons::button(ui, "x", 28.0, true, "Stop").clicked() {
                                self.stop();
                            }
                        } else {
                            let empty = self.composer.trim().is_empty();
                            let send = ui
                                .add_enabled_ui(!empty, |ui| {
                                    crate::icons::button(ui, "send", 28.0, !empty, "Send (Enter)")
                                })
                                .inner;
                            if (send.clicked() || enter) && !empty {
                                // Enter inserted a newline into the text before it was seen.
                                let text = std::mem::take(&mut self.composer);
                                action = Some(PanelAction::Send(text.trim_end().to_string()));
                            }
                        }
                    });
                });
            });
        action
    }
}

/// The images the next message carries, each with a button removing it.
pub fn attachment_chips(ui: &mut egui::Ui, attachments: &mut Vec<Attachment>, tokens: &Tokens) {
    let mut remove = None;
    if attachments.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for (index, attachment) in attachments.iter().enumerate() {
            egui::Frame::new()
                .fill(tokens.chrome)
                .stroke(egui::Stroke::new(1.0, tokens.border))
                .corner_radius(8)
                .inner_margin(4)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Image::new(&attachment.thumbnail)
                                .max_size(egui::vec2(48.0, 36.0))
                                .corner_radius(4),
                        );
                        ui.label(
                            egui::RichText::new(&attachment.label)
                                .small()
                                .color(tokens.text_muted),
                        );
                        if crate::icons::button(ui, "x", 20.0, false, "Remove").clicked() {
                            remove = Some(index);
                        }
                    });
                });
        }
    });
    if let Some(index) = remove {
        attachments.remove(index);
    }
}

/// A chat message as the library stores it: its text, what was sent, and
/// the tool calls and text results of an answer (images are left out).
fn session_message(message: &ChatMessage) -> AiSessionMessage {
    let content = message.display_text();
    let request = message.request_text();
    let results: std::collections::HashMap<&str, bool> = message
        .agent_trace()
        .iter()
        .flat_map(|trace| &trace.content)
        .filter_map(|part| match part {
            AgentContentPart::ToolResult(result) => {
                Some((result.call_id.as_str(), result.is_error))
            }
            _ => None,
        })
        .collect();
    let mut agent_parts = Vec::new();
    for part in message
        .agent_trace()
        .iter()
        .flat_map(|trace| &trace.content)
    {
        match part {
            AgentContentPart::Text(text) => {
                agent_parts.push(AiSessionAgentPart::AssistantText(text.clone()))
            }
            AgentContentPart::ToolCall(call) => agent_parts.push(AiSessionAgentPart::ToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                status: match results.get(call.id.as_str()) {
                    Some(true) => AiSessionToolStatus::Error,
                    Some(false) => AiSessionToolStatus::Completed,
                    None => AiSessionToolStatus::Pending,
                },
            }),
            AgentContentPart::ToolResult(result) => {
                agent_parts.push(AiSessionAgentPart::ToolResult {
                    call_id: result.call_id.clone(),
                    tool_name: result.tool_name.clone(),
                    content: result
                        .content
                        .iter()
                        .filter_map(|part| match part {
                            AgentToolResultPart::Text(text) => {
                                Some(AiSessionToolResultPart::Text(text.clone()))
                            }
                            AgentToolResultPart::Image(_) => None,
                        })
                        .collect(),
                    is_error: result.is_error,
                })
            }
            AgentContentPart::Reasoning(_)
            | AgentContentPart::OpaqueReasoning { .. }
            | AgentContentPart::Image(_) => {}
        }
    }
    AiSessionMessage {
        role: match message.role {
            Role::User => AiSessionRole::User,
            Role::Assistant => AiSessionRole::Assistant,
        },
        content: content.clone(),
        request_content: (request != content).then_some(request),
        references: Vec::new(),
        occurrences: Vec::new(),
        images: Vec::new(),
        agent_parts,
        document_path: message.document_scope().map(|scope| scope.path.clone()),
        document_label: message.document_scope().map(|scope| scope.label.clone()),
    }
}

/// A suffix that differs between conversations, for cache keys.
fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_nanos());
    format!("{nanos:x}")
}

fn chat_message(message: AiSessionMessage) -> ChatMessage {
    let role = match message.role {
        AiSessionRole::User => Role::User,
        AiSessionRole::Assistant => Role::Assistant,
    };
    let scope = message.document_path.map(|path| DocumentContextScope {
        path,
        label: message.document_label.unwrap_or_else(|| "Document".into()),
    });
    ChatMessage::from_persisted(role, message.content, message.request_content)
        .with_document_scope(scope)
}

fn tool_rows(parts: &[AiSessionAgentPart]) -> Vec<ToolRow> {
    parts
        .iter()
        .filter_map(|part| match part {
            AiSessionAgentPart::ToolCall {
                id,
                name,
                arguments,
                status,
            } => Some(ToolRow {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
                state: match status {
                    AiSessionToolStatus::Completed => ToolState::Done,
                    _ => ToolState::Failed,
                },
            }),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::fake_providers::{FAKE_ANSWER, FAKE_SEARCH, start_ai_server, use_fake_ai};

    #[test]
    fn quick_questions_start_afresh_and_continue_in_the_assistant() {
        let ctx = egui::Context::default();
        // No model is set up, so questions stay without answers.
        let mut settings = Settings::default();
        settings
            .ai
            .provider_config_mut(AiProvider::OpenAi)
            .api_key
            .clear();
        let ask = || AskContext {
            path: "doc.pdf".into(),
            title: "doc.pdf".into(),
            layout: DocumentLayout::Fixed,
            page_count: 1,
            current_page: 0,
            page_text: None,
            selection: None,
        };
        let mut quick = Assistant::default();
        quick.send("First?".into(), ask(), &settings, &ctx);
        quick.new_session();
        quick.send("Second?".into(), ask(), &settings, &ctx);
        assert_eq!(quick.messages().len(), 1, "no history");
        assert_eq!(quick.messages()[0].display_text(), "Second?");

        let mut assistant = Assistant::default();
        assistant.send("Earlier".into(), ask(), &settings, &ctx);
        assert!(assistant.adopt(&mut quick));
        assert_eq!(assistant.messages().len(), 1, "a new chat of the quick one");
        assert_eq!(assistant.messages()[0].display_text(), "Second?");
        assert!(quick.messages().is_empty());
    }

    #[test]
    fn attached_region_images_go_with_the_next_message_only() {
        let ctx = egui::Context::default();
        let mut assistant = Assistant::default();
        let thumbnail = ctx.load_texture(
            "region",
            egui::ColorImage::filled([2, 2], egui::Color32::RED),
            egui::TextureOptions::LINEAR,
        );
        let reference = glyphst_core::RegionImageReference::new([glyphst_core::PageRegion {
            page: glyphst_core::PageIndex(0),
            x0: 0.0,
            y0: 0.0,
            x1: 10.0,
            y1: 10.0,
        }]);
        assistant.attachments.push(Attachment {
            image: ImageContext::new_region(reference, "image/png", vec![1, 2, 3]),
            thumbnail,
            label: "Region of page 1".into(),
        });
        // No model is set up, so the message stays without an answer.
        let mut settings = Settings::default();
        settings
            .ai
            .provider_config_mut(AiProvider::OpenAi)
            .api_key
            .clear();
        let ask = || AskContext {
            path: "doc.pdf".into(),
            title: "doc.pdf".into(),
            layout: DocumentLayout::Fixed,
            page_count: 1,
            current_page: 0,
            page_text: None,
            selection: None,
        };
        assistant.send("What is this?".into(), ask(), &settings, &ctx);
        let images = |message: &ChatMessage| {
            message
                .into_agent_messages()
                .iter()
                .flat_map(|message| &message.content)
                .filter(|part| matches!(part, AgentContentPart::Image(_)))
                .count()
        };
        assert_eq!(images(&assistant.messages()[0]), 1);
        assert!(assistant.attachments.is_empty());
        assert_eq!(assistant.sent_images.get(&0).map(Vec::len), Some(1));
        assistant.send("And now?".into(), ask(), &settings, &ctx);
        assert_eq!(images(&assistant.messages()[1]), 0, "sent once");
    }

    #[test]
    fn a_turn_streams_uses_the_tools_and_is_saved_as_a_library_session() {
        let directory =
            std::env::temp_dir().join(format!("glyphst-reader-assistant-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("doc.pdf");
        std::fs::write(
            &path,
            glyphst_pdf::test_pdf::text_pdf(&["Here the story begins.", "And the story ends."]),
        )
        .unwrap();
        let mut library = LibraryStore::open_for_test(directory.join("library.redb")).unwrap();
        let mut settings = Settings::default();
        use_fake_ai(&mut settings, start_ai_server().unwrap());

        let ctx = egui::Context::default();
        let mut assistant = Assistant::default();
        let ask = AskContext {
            path: path.clone(),
            title: "doc.pdf".into(),
            layout: DocumentLayout::Fixed,
            page_count: 1,
            current_page: 0,
            page_text: Some("Here the story begins.".into()),
            selection: None,
        };
        assistant.send("Where does the story end?".into(), ask, &settings, &ctx);
        let started = Instant::now();
        while assistant.is_running() {
            assert!(started.elapsed() < Duration::from_secs(30), "timed out");
            assistant.poll(Some(&mut library));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(assistant.error, None);
        assert_eq!(assistant.messages().len(), 2);
        assert_eq!(assistant.messages()[1].display_text(), FAKE_ANSWER);
        let rows = assistant.tools(1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label(), format!("Searched for “{FAKE_SEARCH}”"));
        assert_eq!(rows[0].state, ToolState::Done);
        assert!(
            assistant.messages()[0]
                .request_text()
                .contains("Here the story begins."),
            "the page goes with the question"
        );

        // The session is in the library and opens again.
        let mut reopened = Assistant::default();
        reopened.refresh_sessions(&library);
        assert_eq!(reopened.sessions.len(), 1);
        assert_eq!(reopened.sessions[0].title, "Where does the story end?");
        let id = reopened.sessions[0].id;
        reopened.load_session(id);
        assert_eq!(reopened.messages()[1].display_text(), FAKE_ANSWER);
        assert_eq!(reopened.tools(1)[0].label(), rows[0].label());

        // Without a usable model the question stays and an error explains.
        let mut unconfigured = Assistant::default();
        let mut missing = Settings::default();
        missing
            .ai
            .provider_config_mut(AiProvider::OpenAi)
            .api_key
            .clear();
        let ask = AskContext {
            path,
            title: "doc.pdf".into(),
            layout: DocumentLayout::Fixed,
            page_count: 1,
            current_page: 0,
            page_text: None,
            selection: None,
        };
        unconfigured.send("Hello?".into(), ask, &missing, &ctx);
        assert!(!unconfigured.is_running());
        assert!(
            unconfigured
                .error
                .as_deref()
                .is_some_and(|error| error.contains("Settings > AI"))
        );
        drop(library);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
