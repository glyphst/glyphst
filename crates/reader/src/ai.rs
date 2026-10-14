//! The assistant: a chat about the open document in a right-side panel.
//!
//! Turns run the `ai` crate's agent on a worker thread with the app's tools;
//! the current page and selection can be attached as context, and each
//! message ends with what the reader showed. Sessions are the library's AI
//! sessions, saved with their whole trace (reasoning, tool calls and
//! results, images), so a reopened chat continues where it was. Long chats
//! are summarized before the context fills up, and a model names each new
//! chat. The panel follows pdfcraft's side panels; answers are Markdown.

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
        AgentCancellationToken, AgentContentPart, AgentEvent, AgentImage, AgentLimits,
        AgentMessage, AgentRole, AgentRun, AgentStopReason, AgentToolCall, AgentToolResult,
        AgentToolResultPart, run_agent,
    },
    ai_chat::{ChatMessage, ContextBlock, DocumentContextScope, ImageContext, MentionKind, Role},
    compaction,
    prompt::{DocumentFacts, ReaderState},
    tools::{ToolContext, ToolRegistry},
};
use glyphst_core::DocumentLayout;
use glyphst_library::{
    AiSession, AiSessionAgentPart, AiSessionImage, AiSessionMessage, AiSessionRole,
    AiSessionSummary, AiSessionToolResultPart, AiSessionToolStatus, LibraryStore,
    current_timestamp,
};
use glyphst_settings::{
    AiApiStyle, AiProvider, AiThinkingLevel, ModelRef, ResolvedAiConfig, Settings,
};

use crate::{
    ai_changes::{ChangeAction, ChangeLog},
    ai_tools::describe_call,
    theme::Tokens,
};

const TITLE_CHARS: usize = 60;

/// What a turn knows about the open document.
pub struct AskContext {
    pub path: PathBuf,
    /// The id tools take for the document.
    pub id: String,
    pub title: String,
    pub authors: Option<String>,
    /// The outline entry of the current page.
    pub section: Option<String>,
    /// The other open documents.
    pub others: Vec<DocumentFacts>,
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
    /// The conversation before the new message was summarized.
    Compacted(String),
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
    /// What stands for the start of a long conversation.
    summary: Option<AiSessionSummary>,
    /// Input tokens of the latest turn, and its model.
    context_tokens: (u64, String),
    /// The next turn summarizes the conversation first.
    compact_next: bool,
    /// The chat's title, once a model named it.
    title: Option<String>,
    /// A title being written, and the model writing it.
    title_job: Option<Receiver<String>>,
    title_config: Option<ResolvedAiConfig>,
    /// Names this conversation in the change log.
    chat_id: u64,
    /// The user message whose "Restore to here" waits for confirmation.
    confirm_restore: Option<usize>,
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
            summary: None,
            context_tokens: (0, String::new()),
            compact_next: false,
            title: None,
            title_job: None,
            title_config: None,
            chat_id: 0,
            confirm_restore: None,
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
    /// Review or undo what agents changed.
    Change(ChangeAction),
    /// Rewind the chat to the user message at this index and undo what the
    /// agent changed since.
    Restore(usize),
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
        tools: ToolRegistry,
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
                id: Some(ask.id.clone()),
                title: ask.title.clone(),
                format: match ask.layout {
                    DocumentLayout::Fixed => "PDF",
                    DocumentLayout::Reflow(_) => "EPUB",
                }
                .into(),
                authors: ask.authors.clone(),
                page_count: ask.page_count,
            }),
            current_page: Some(page),
            section: ask.section.clone(),
            selection_chars: ask.selection.as_ref().map(|text| text.chars().count()),
            other_documents: ask.others.clone(),
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
        let question = self.messages.len() - 1;
        let prior = self.history(question);
        let new = self.messages[question].into_agent_messages();
        let compact = std::mem::take(&mut self.compact_next) && !prior.is_empty();
        if compact {
            self.status = Some("Summarizing the conversation so far…".into());
        }
        if self.title.is_none() && question == 0 {
            self.title_config = Some(settings.ai.resolved_title().unwrap_or_else(|| {
                let mut config = config.clone();
                config.thinking_level = AiThinkingLevel::Off;
                config
            }));
        }
        self.messages.push(ChatMessage::assistant(String::new()));
        self.tools.push(Vec::new());
        let cache_key = self
            .cache_key
            .get_or_insert_with(|| format!("glyphst-chat-{}", unique_suffix()))
            .clone();
        self.context_tokens.1 = config.model.clone();
        if self.chat_id == 0 {
            self.chat_id = next_chat_id();
        }
        let tool_context = ToolContext {
            thread: Some(self.chat_id),
            turn: question,
            ..ToolContext::default()
        };
        let cancel = AgentCancellationToken::default();
        let (events_tx, events) = mpsc::channel();
        let (worker_cancel, ctx) = (cancel.clone(), ctx.clone());
        thread::spawn(move || {
            let mut history = prior;
            if compact {
                // A failed summary leaves the conversation as it was.
                if let Ok(summary) =
                    compaction::summarize(config.clone(), history.clone(), worker_cancel.clone())
                {
                    history = vec![compaction::summary_message(&summary)];
                    let _ = events_tx.send(RunEvent::Compacted(summary));
                    ctx.request_repaint();
                }
            }
            history.extend(new);
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
                        ..tool_context
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

    /// The messages before `end` as the model reads them: the summary of
    /// the conversation's start, then the rest.
    fn history(&self, end: usize) -> Vec<AgentMessage> {
        let start = self
            .summary
            .as_ref()
            .map_or(0, |summary| summary.until.min(end));
        let mut history = self
            .summary
            .iter()
            .map(|summary| compaction::summary_message(&summary.text))
            .collect::<Vec<_>>();
        history.extend(
            self.messages[start..end]
                .iter()
                .flat_map(ChatMessage::into_agent_messages),
        );
        history
    }

    /// Applies the running turn's progress; saves the session when it ends.
    pub fn poll(&mut self, mut library: Option<&mut LibraryStore>) {
        if let Some(job) = &self.title_job
            && let Ok(title) = job.try_recv()
        {
            self.title = Some(title);
            self.title_job = None;
            if let Some(library) = library.as_deref_mut() {
                self.save(library);
            }
        }
        let Some(run) = &self.run else {
            return;
        };
        let answer = run.answer;
        let events: Vec<RunEvent> = run.events.try_iter().collect();
        let mut finished = false;
        for event in events {
            match event {
                RunEvent::Agent(event) => self.apply(answer, event),
                RunEvent::Compacted(text) => {
                    self.summary = Some(AiSessionSummary {
                        text,
                        until: answer - 1,
                    });
                    self.status = None;
                }
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
            let (tokens, model) = &self.context_tokens;
            self.compact_next = compaction::needs_compaction(model, *tokens);
            if let Some(library) = library {
                // Only saved chats get a title.
                self.start_title(answer);
                self.save(library);
            }
        }
    }

    /// Asks a model for the chat's title after its first answer.
    fn start_title(&mut self, answer: usize) {
        let Some(config) = self.title_config.take() else {
            return;
        };
        let question = self.messages[0].display_text();
        let answer = self.messages[answer].display_text();
        if answer.trim().is_empty() {
            return;
        }
        let (titles, job) = mpsc::channel();
        thread::spawn(move || {
            if let Ok(title) = compaction::generate_title(
                config,
                &question,
                &answer,
                AgentCancellationToken::default(),
            ) {
                let _ = titles.send(title);
            }
        });
        self.title_job = Some(job);
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
                stop_reason, usage, ..
            } => {
                self.context_tokens.0 = usage.input_tokens + usage.output_tokens;
                if stop_reason == AgentStopReason::Refusal {
                    self.error = Some("The model declined this request.".into());
                }
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
        self.summary = None;
        self.context_tokens = (0, String::new());
        self.compact_next = false;
        self.title = None;
        self.title_job = None;
        self.title_config = None;
        self.chat_id = 0;
        self.confirm_restore = None;
        self.error = None;
        self.show_sessions = false;
    }

    /// Names this conversation in the change log.
    pub fn chat_id(&self) -> u64 {
        self.chat_id
    }

    /// Rewinds the chat to before the user message at `index`; returns the
    /// message's text for the composer.
    pub fn rewind(&mut self, index: usize) -> Option<String> {
        let message = self.messages.get(index)?;
        if message.role != Role::User {
            return None;
        }
        let text = message.display_text();
        self.stop();
        self.run = None;
        self.status = None;
        self.error = None;
        self.confirm_restore = None;
        self.compact_next = false;
        self.messages.truncate(index);
        self.tools.truncate(index);
        self.reasoning.retain(|&answer, _| answer < index);
        self.sent_images.retain(|&message, _| message < index);
        if self
            .summary
            .as_ref()
            .is_some_and(|summary| summary.until > index)
        {
            self.summary = None;
        }
        Some(text)
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
        self.summary = other.summary.take();
        self.context_tokens = std::mem::take(&mut other.context_tokens);
        self.compact_next = other.compact_next;
        self.chat_id = other.chat_id;
        other.new_session();
        true
    }

    /// Opens a saved chat to continue it.
    pub fn load_session(&mut self, id: u64, library: &LibraryStore) {
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
        self.title = Some(session.title);
        self.summary = session.summary;
        for message in session.messages {
            let index = self.messages.len();
            self.tools.push(tool_rows(&message.agent_parts));
            let reasoning = message
                .agent_parts
                .iter()
                .filter_map(|part| match part {
                    AiSessionAgentPart::Reasoning(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            if !reasoning.is_empty() {
                self.reasoning.insert(index, reasoning);
            }
            self.messages.push(chat_message(message, library));
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
        let title = self.title.clone().unwrap_or_else(|| {
            self.messages
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
                .unwrap_or_else(|| "Chat".into())
        });
        let messages = self
            .messages
            .iter()
            .map(|message| session_message(message, library))
            .collect();
        let session = AiSession {
            id: self.session_id,
            title,
            messages,
            queued_messages: Vec::new(),
            created: self.created,
            updated: now,
            summary: self.summary.clone(),
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
        changes: &mut ChangeLog,
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
                                        ui.set_max_width(width * 0.8);
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
                                if self.run.is_none()
                                    && crate::icons::button(
                                        ui,
                                        "rotate-ccw",
                                        22.0,
                                        false,
                                        "Restore to here: rewind the chat to this message and undo what the agent changed since",
                                    )
                                    .clicked()
                                {
                                    self.confirm_restore = Some(index);
                                }
                            });
                            if self.confirm_restore == Some(index) {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        egui::RichText::new(
                                            "Rewind to this message and undo the agent's changes since?",
                                        )
                                        .small()
                                        .color(tokens.text_muted),
                                    );
                                    if ui.small_button("Restore").clicked() {
                                        action = Some(PanelAction::Restore(index));
                                    }
                                    if ui.small_button("Cancel").clicked() {
                                        self.confirm_restore = None;
                                    }
                                });
                            }
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
        if let Some(change) = changes.ui(ui, &tokens) {
            action = Some(PanelAction::Change(change));
        }
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

/// A chat message as the library stores it: its text, what was sent with
/// its images, and an answer's whole trace (reasoning, tool calls and
/// results with their images, and the provider's reasoning state).
fn session_message(message: &ChatMessage, library: &mut LibraryStore) -> AiSessionMessage {
    let content = message.display_text();
    let request = message.request_text();
    let mut install = |image: &AgentImage| -> Option<AiSessionImage> {
        let (width, height) = image_size(image)?;
        library
            .install_ai_image(
                image.name.as_deref(),
                &image.media_type,
                width,
                height,
                &image.bytes,
            )
            .ok()
    };
    let images = message
        .into_agent_messages()
        .iter()
        .filter(|_| message.role == Role::User)
        .flat_map(|trace| &trace.content)
        .filter_map(|part| match part {
            AgentContentPart::Image(image) => install(image),
            _ => None,
        })
        .collect();
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
        agent_parts.push(match part {
            AgentContentPart::Text(text) => AiSessionAgentPart::AssistantText(text.clone()),
            AgentContentPart::Reasoning(text) => AiSessionAgentPart::Reasoning(text.clone()),
            AgentContentPart::ToolCall(call) => AiSessionAgentPart::ToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                status: match results.get(call.id.as_str()) {
                    Some(true) => AiSessionToolStatus::Error,
                    Some(false) => AiSessionToolStatus::Completed,
                    None => AiSessionToolStatus::Pending,
                },
            },
            AgentContentPart::ToolResult(result) => AiSessionAgentPart::ToolResult {
                call_id: result.call_id.clone(),
                tool_name: result.tool_name.clone(),
                content: result
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        AgentToolResultPart::Text(text) => {
                            Some(AiSessionToolResultPart::Text(text.clone()))
                        }
                        AgentToolResultPart::Image(image) => {
                            install(image).map(AiSessionToolResultPart::Image)
                        }
                    })
                    .collect(),
                is_error: result.is_error,
            },
            AgentContentPart::OpaqueReasoning { api_style, value } => {
                AiSessionAgentPart::OpaqueReasoning {
                    api_style: api_style.as_str().to_string(),
                    value: value.to_string(),
                }
            }
            AgentContentPart::Image(_) => continue,
        });
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
        images,
        agent_parts,
        document_path: message.document_scope().map(|scope| scope.path.clone()),
        document_label: message.document_scope().map(|scope| scope.label.clone()),
    }
}

/// An id for a new conversation, unique while the app runs.
fn next_chat_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A suffix that differs between conversations, for cache keys.
fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_nanos());
    format!("{nanos:x}")
}

/// An image's width and height, read from its header.
fn image_size(image: &AgentImage) -> Option<(u32, u32)> {
    if image.width > 0 && image.height > 0 {
        return Some((image.width, image.height));
    }
    image::ImageReader::new(std::io::Cursor::new(image.bytes.as_ref()))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// A saved message as the chat shows and continues it.
fn chat_message(message: AiSessionMessage, library: &LibraryStore) -> ChatMessage {
    let role = match message.role {
        AiSessionRole::User => Role::User,
        AiSessionRole::Assistant => Role::Assistant,
    };
    let scope = message.document_path.map(|path| DocumentContextScope {
        path,
        label: message.document_label.unwrap_or_else(|| "Document".into()),
    });
    let load = |image: &AiSessionImage| {
        library.load_ai_image(image).ok().map(|bytes| {
            AgentImage::new(image.media_type.clone(), image.width, image.height, bytes)
                .with_optional_name(image.name.clone())
        })
    };
    let mut chat = ChatMessage::from_persisted(role, message.content, message.request_content)
        .with_document_scope(scope);
    for image in message.images.iter().filter_map(load) {
        chat.attach_user_image(image);
    }
    let trace = agent_trace(&message.agent_parts, load);
    if !trace.is_empty() {
        chat.set_agent_trace(trace);
    }
    chat
}

/// The agent messages an answer's saved parts came from: assistant turns,
/// each followed by its tool results.
fn agent_trace(
    parts: &[AiSessionAgentPart],
    load: impl Fn(&AiSessionImage) -> Option<AgentImage>,
) -> Vec<AgentMessage> {
    let mut trace: Vec<AgentMessage> = Vec::new();
    for part in parts {
        let content = match part {
            AiSessionAgentPart::ToolResult {
                call_id,
                tool_name,
                content,
                is_error,
            } => {
                trace.push(AgentMessage::tool_result(AgentToolResult {
                    call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    content: content
                        .iter()
                        .filter_map(|part| match part {
                            AiSessionToolResultPart::Text(text) => {
                                Some(AgentToolResultPart::Text(text.clone()))
                            }
                            AiSessionToolResultPart::Image(image) => {
                                load(image).map(AgentToolResultPart::Image)
                            }
                        })
                        .collect(),
                    is_error: *is_error,
                }));
                continue;
            }
            AiSessionAgentPart::AssistantText(text) => AgentContentPart::Text(text.clone()),
            AiSessionAgentPart::Reasoning(text) => AgentContentPart::Reasoning(text.clone()),
            AiSessionAgentPart::ToolCall {
                id,
                name,
                arguments,
                ..
            } => AgentContentPart::ToolCall(AgentToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            }),
            AiSessionAgentPart::OpaqueReasoning { api_style, value } => {
                let (Some(api_style), Ok(value)) =
                    (AiApiStyle::parse(api_style), serde_json::from_str(value))
                else {
                    continue;
                };
                AgentContentPart::OpaqueReasoning { api_style, value }
            }
        };
        match trace.last_mut() {
            Some(message) if message.role == AgentRole::Assistant => message.content.push(content),
            _ => trace.push(AgentMessage {
                role: AgentRole::Assistant,
                content: vec![content],
            }),
        }
    }
    trace
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
    use crate::fake_providers::{
        FAKE_ANSWER, FAKE_SEARCH, FAKE_SUMMARY, FAKE_TITLE, start_ai_server, use_fake_ai,
    };

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
            id: "doc_1".into(),
            title: "doc.pdf".into(),
            authors: None,
            section: None,
            others: Vec::new(),
            layout: DocumentLayout::Fixed,
            page_count: 1,
            current_page: 0,
            page_text: None,
            selection: None,
        };
        let mut quick = Assistant::default();
        quick.send("First?".into(), ask(), ToolRegistry::new(), &settings, &ctx);
        quick.new_session();
        quick.send(
            "Second?".into(),
            ask(),
            ToolRegistry::new(),
            &settings,
            &ctx,
        );
        assert_eq!(quick.messages().len(), 1, "no history");
        assert_eq!(quick.messages()[0].display_text(), "Second?");

        let mut assistant = Assistant::default();
        assistant.send(
            "Earlier".into(),
            ask(),
            ToolRegistry::new(),
            &settings,
            &ctx,
        );
        assistant.send("Later".into(), ask(), ToolRegistry::new(), &settings, &ctx);
        assert_eq!(assistant.messages().len(), 2);
        assert_eq!(assistant.rewind(1).as_deref(), Some("Later"));
        assert_eq!(assistant.messages().len(), 1);
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
            id: "doc_1".into(),
            title: "doc.pdf".into(),
            authors: None,
            section: None,
            others: Vec::new(),
            layout: DocumentLayout::Fixed,
            page_count: 1,
            current_page: 0,
            page_text: None,
            selection: None,
        };
        assistant.send(
            "What is this?".into(),
            ask(),
            ToolRegistry::new(),
            &settings,
            &ctx,
        );
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
        assistant.send(
            "And now?".into(),
            ask(),
            ToolRegistry::new(),
            &settings,
            &ctx,
        );
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
            id: "doc_1".into(),
            title: "doc.pdf".into(),
            authors: None,
            section: None,
            others: Vec::new(),
            layout: DocumentLayout::Fixed,
            page_count: 1,
            current_page: 0,
            page_text: Some("Here the story begins.".into()),
            selection: None,
        };
        let documents = crate::ai_tools::DocumentDirectory::default();
        documents.update(
            vec![crate::ai_tools::OpenDocument {
                id: "doc_1".into(),
                title: "doc.pdf".into(),
                path: path.clone(),
                layout: DocumentLayout::Fixed,
                page_count: 1,
                current_page: 0,
            }],
            Some("doc_1".into()),
        );
        let mut tools = ToolRegistry::new();
        documents.register(&mut tools);
        assistant.send(
            "Where does the story end?".into(),
            ask,
            tools,
            &settings,
            &ctx,
        );
        let started = Instant::now();
        while assistant.is_running() {
            assert!(started.elapsed() < Duration::from_secs(30), "timed out");
            assistant.poll(Some(&mut library));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(assistant.error, None);
        assert_eq!(assistant.messages().len(), 2);
        assert_eq!(assistant.messages()[1].display_text(), FAKE_ANSWER);
        let rows = assistant.tools(1).to_vec();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label(), format!("Searched for “{FAKE_SEARCH}”"));
        assert_eq!(rows[0].state, ToolState::Done);
        assert!(
            assistant.messages()[0]
                .request_text()
                .contains("Here the story begins."),
            "the page goes with the question"
        );

        // A model names the chat once it is answered.
        let started = Instant::now();
        while assistant.title.is_none() {
            assert!(started.elapsed() < Duration::from_secs(30), "no title");
            assistant.poll(Some(&mut library));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(assistant.title.as_deref(), Some(FAKE_TITLE));

        // The session is in the library and opens again with its trace.
        let mut reopened = Assistant::default();
        reopened.refresh_sessions(&library);
        assert_eq!(reopened.sessions.len(), 1);
        assert_eq!(reopened.sessions[0].title, FAKE_TITLE);
        let id = reopened.sessions[0].id;
        reopened.load_session(id, &library);
        assert_eq!(reopened.messages()[1].display_text(), FAKE_ANSWER);
        assert_eq!(reopened.tools(1)[0].label(), rows[0].label());
        assert_eq!(
            reopened.history(2),
            assistant.history(2),
            "a reopened chat continues with the same messages"
        );
        let trace = reopened.messages()[1].agent_trace();
        assert_eq!(trace.len(), 3, "the tool call, its result and the answer");
        assert_eq!(trace[1].role, AgentRole::Tool);

        // A chat near the end of the context is summarized first.
        reopened.compact_next = true;
        let ask = AskContext {
            path: path.clone(),
            id: "doc_1".into(),
            title: "doc.pdf".into(),
            authors: None,
            section: None,
            others: Vec::new(),
            layout: DocumentLayout::Fixed,
            page_count: 1,
            current_page: 0,
            page_text: None,
            selection: None,
        };
        reopened.send(
            "And then?".into(),
            ask,
            ToolRegistry::new(),
            &settings,
            &ctx,
        );
        let started = Instant::now();
        while reopened.is_running() {
            assert!(started.elapsed() < Duration::from_secs(30), "timed out");
            reopened.poll(Some(&mut library));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(reopened.error, None);
        let summary = reopened.summary.clone().expect("a summary");
        assert_eq!(summary.text, FAKE_SUMMARY);
        assert_eq!(summary.until, 2);
        let history = reopened.history(4);
        assert!(history[0].text().contains(FAKE_SUMMARY));
        assert!(history[1].text().starts_with("And then?"));
        assert!(
            history[2..]
                .iter()
                .all(|message| message.role != AgentRole::User),
            "then the answer's trace, and nothing from before the summary"
        );

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
            id: "doc_1".into(),
            title: "doc.pdf".into(),
            authors: None,
            section: None,
            others: Vec::new(),
            layout: DocumentLayout::Fixed,
            page_count: 1,
            current_page: 0,
            page_text: None,
            selection: None,
        };
        unconfigured.send("Hello?".into(), ask, ToolRegistry::new(), &missing, &ctx);
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
