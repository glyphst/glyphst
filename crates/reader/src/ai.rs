//! The assistant: a chat about the open document in a right-side panel.
//!
//! Turns run the `ai` crate's agent on a worker thread with the app's tools,
//! or an external coding agent; messages carry `@` mentions and pictures as
//! chips, and each ends with what the reader showed. Sessions are the
//! library's AI sessions, saved with their whole trace (reasoning, tool
//! calls and results, images) and the chips as typed, so a reopened chat
//! continues where it was. Long chats are summarized before the context
//! fills up, and a model names each new chat.
//!
//! The panel ([`header`], [`thread`], [`composer`]) follows Zed's agent
//! panel for the composer and Lody's and Zeron's threads: answers are
//! Markdown after a quiet timeline of the work, folded once done.

mod composer;
mod demo;
mod header;
mod thread;

pub use demo::DEMO_STATES;

use std::{
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    thread as threads,
    time::Instant,
};

use glyphst_agent_host::{
    external::{AgentSpec, ExternalEvent, ExternalTurn, McpAccess},
    scratch::{FileChanges, Scratch},
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
    tools::{ToolContext, ToolOrigin, ToolRegistry},
};
use glyphst_core::DocumentLayout;
use glyphst_library::{
    AiSession, AiSessionAgent, AiSessionAgentPart, AiSessionImage, AiSessionMention,
    AiSessionMessage, AiSessionRole, AiSessionSummary, AiSessionToolResultPart,
    AiSessionToolStatus, LibraryStore, current_timestamp,
};
use glyphst_settings::{
    AiApiStyle, AiProvider, AiThinkingLevel, ModelRef, ResolvedAiConfig, Settings,
};

use crate::{
    ai_changes::{ChangeAction, ChangeLog},
    ai_editor::{ChipKind, Draft, MentionEditor},
    ai_mentions::{self, Mention, Sources},
    ai_menu::Menu,
    ai_preview::Previews,
    ai_tools::{Step, describe_step},
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
    /// What it returned, for its details.
    pub output: Option<String>,
}

impl ToolRow {
    pub fn step(&self) -> Step {
        describe_step(&self.name, &self.arguments)
    }

    #[cfg(test)]
    pub fn label(&self) -> String {
        self.step().done()
    }
}

/// A part of an answer, in the order it came.
#[derive(Clone, Debug, PartialEq)]
pub enum Segment {
    Thought(String),
    Tool(ToolRow),
    Text(String),
}

/// What the panel keeps about a message besides what the model reads.
#[derive(Default)]
struct MessageView {
    /// A user message as typed, with its chips.
    draft: Option<Draft>,
    /// Thumbnails of the pictures a user message carried.
    images: Vec<egui::TextureHandle>,
    /// Pictures of a reopened message, made thumbnails when first drawn.
    pending_images: Vec<AgentImage>,
    /// When a user message was sent, in seconds since the epoch.
    sent_at: Option<u64>,
    /// An answer's reasoning, tool calls and text, in order.
    segments: Vec<Segment>,
    /// How long an answer took.
    duration_ms: Option<u64>,
    /// The work before a finished answer is shown, not folded.
    unfolded: bool,
    /// Steps whose details are open, by segment.
    open_steps: std::collections::HashSet<usize>,
}

impl MessageView {
    fn push_text(&mut self, text: &str) {
        match self.segments.last_mut() {
            Some(Segment::Text(last)) => last.push_str(text),
            _ => self.segments.push(Segment::Text(text.to_string())),
        }
    }

    fn push_reasoning(&mut self, text: &str) {
        match self.segments.last_mut() {
            Some(Segment::Thought(last)) => last.push_str(text),
            _ => self.segments.push(Segment::Thought(text.to_string())),
        }
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut ToolRow> {
        self.segments.iter_mut().find_map(|segment| match segment {
            Segment::Tool(row) if row.id == id => Some(row),
            _ => None,
        })
    }

    fn tools(&self) -> impl Iterator<Item = &ToolRow> {
        self.segments.iter().filter_map(|segment| match segment {
            Segment::Tool(row) => Some(row),
            _ => None,
        })
    }

    /// Where the answer starts: after the last thought or tool call.
    fn answer_start(&self) -> usize {
        self.segments
            .iter()
            .rposition(|segment| !matches!(segment, Segment::Text(_)))
            .map_or(0, |index| index + 1)
    }
}

enum RunEvent {
    Agent(AgentEvent),
    /// The conversation before the new message was summarized.
    Compacted(String),
    /// The new message with its mentions read.
    Prepared(ChatMessage),
    /// Progress of an external agent's turn.
    External(ExternalEvent),
    /// Files an external agent changed in its working folder.
    Files(Scratch, FileChanges),
    Finished(Vec<AgentMessage>),
    Error(String),
}

struct Run {
    cancel: AgentCancellationToken,
    events: Receiver<RunEvent>,
    /// The answer being written.
    answer: usize,
    started: Instant,
}

/// A sent message being edited in place.
struct Editing {
    index: usize,
    editor: MentionEditor,
}

pub struct Assistant {
    pub open: bool,
    session_id: u64,
    created: u64,
    messages: Vec<ChatMessage>,
    /// What the panel shows of each message, by index.
    views: Vec<MessageView>,
    /// The composer's text and chips.
    pub editor: MentionEditor,
    /// The composer's `@`, `/` and `+` menu.
    menu: Menu,
    previews: Previews,
    /// Messages written while an answer runs, sent in turn after it.
    queue: Vec<Draft>,
    /// The page and selection go with each message as context (for quick
    /// questions; the panel attaches them as chips).
    pub include_page: bool,
    pub include_selection: bool,
    run: Option<Run>,
    pub error: Option<String>,
    markdown: egui_commonmark::CommonMarkCache,
    show_sessions: bool,
    sessions: Vec<AiSession>,
    sessions_query: String,
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
    editing: Option<Editing>,
    /// The composer takes most of the panel.
    expanded: bool,
    /// The model and agent menu is open.
    selector_open: bool,
    /// The `…` menu is open.
    more_open: bool,
    /// A picture being read from the clipboard.
    paste_job: Option<Receiver<Vec<u8>>>,
    /// The id of the external agent the chat runs on; `None` for the
    /// built-in assistant.
    pub agent: Option<String>,
    /// The external agent's own session, to continue.
    agent_session: Option<String>,
    /// Files external agents changed, for the app's change log.
    file_changes: Vec<(ToolContext, Scratch, FileChanges)>,
    /// The tool context of the running turn.
    run_context: ToolContext,
    /// Who answers: the model's or the agent's name.
    answerer: String,
    /// The composer's height last frame, to lay out the thread above it.
    composer_height: f32,
    /// The "Scroll to bottom" pill shows.
    pill: bool,
    jump_to_bottom: bool,
    /// A canned state for screenshots; holds its never-ending run open.
    demo: Option<mpsc::Sender<RunEvent>>,
    /// The canned state shows the composer's first chip's preview.
    demo_hover: bool,
}

/// What an external agent's turn needs from the app.
pub struct ExternalAccess {
    pub agent: AgentSpec,
    pub scratch: Scratch,
    pub mcp: McpAccess,
    pub model: Option<String>,
}

/// An image attached to the next message.
#[derive(Clone)]
pub struct Attachment {
    pub image: AttachedImage,
    pub thumbnail: egui::TextureHandle,
    pub label: String,
}

impl std::fmt::Debug for Attachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attachment")
            .field("image", &self.image)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// What an attached image is.
#[derive(Clone, Debug)]
pub enum AttachedImage {
    /// A region of a page, drawn with the region tool.
    Region(ImageContext),
    /// A picture the user pasted or dropped.
    Picture(AgentImage),
}

/// Longest side of an attachment's thumbnail, in pixels.
const THUMBNAIL_PIXELS: u32 = 160;
/// Pictures larger than this are refused.
const MAX_PICTURE_BYTES: usize = 16 * 1024 * 1024;

impl Default for Assistant {
    fn default() -> Self {
        Self {
            open: false,
            session_id: 0,
            created: 0,
            messages: Vec::new(),
            views: Vec::new(),
            editor: MentionEditor::new(egui::Id::new("assistant-input")),
            menu: Menu::default(),
            previews: Previews::default(),
            queue: Vec::new(),
            include_page: false,
            include_selection: false,
            run: None,
            error: None,
            markdown: egui_commonmark::CommonMarkCache::default(),
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
            editing: None,
            expanded: false,
            selector_open: false,
            more_open: false,
            paste_job: None,
            agent: None,
            agent_session: None,
            file_changes: Vec::new(),
            run_context: ToolContext::default(),
            answerer: String::new(),
            composer_height: 110.0,
            pill: false,
            jump_to_bottom: false,
            demo: None,
            demo_hover: false,
            show_sessions: false,
            sessions: Vec::new(),
            sessions_query: String::new(),
        }
    }
}

/// What the panel shows besides the chat.
pub struct Panel<'a> {
    /// The active document's title, current page (from 0, once its text
    /// is read) and selected words, for the composer's suggestions.
    pub document: Option<String>,
    pub page: Option<usize>,
    pub selection_words: Option<usize>,
    pub models: &'a [(ModelRef, String)],
    pub model: Option<ModelRef>,
    pub changes: &'a mut ChangeLog,
    /// What the `@` menu offers, gathered when it opens.
    pub sources: &'a mut dyn FnMut() -> Sources,
    /// The external agents installed or configured.
    pub agents: &'a [AgentSpec],
}

/// What the panel asks for.
#[derive(Debug)]
pub enum PanelAction {
    Send(Draft),
    NewSession,
    LoadSession(u64),
    DeleteSession(u64),
    /// The user chose the assistant's model.
    SetModel(ModelRef),
    /// Review or undo what agents changed.
    Change(ChangeAction),
    /// Rewind the chat to the user message at this index and undo what the
    /// agent changed since; with a draft, send it in its place.
    Restore(usize, Option<Draft>),
    /// Run new chats on the external agent with this id, or the built-in
    /// assistant.
    SetAgent(Option<String>),
    /// Choose a picture file to attach.
    ChoosePicture,
    /// Draw a region of a page to attach.
    SelectRegion,
    Command(crate::commands::Command),
    OpenAiSettings,
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
    pub fn tools(&self, message: usize) -> Vec<ToolRow> {
        self.views
            .get(message)
            .map(|view| view.tools().cloned().collect())
            .unwrap_or_default()
    }

    pub fn refresh_sessions(&mut self, library: &LibraryStore) {
        let mut sessions = library.ai_sessions();
        sessions.sort_by_key(|session| std::cmp::Reverse(session.updated));
        self.sessions = sessions;
    }

    /// Starts a turn: the message with its chips read, then the agent with
    /// the document tools. While one runs, the message waits its turn.
    pub fn send(
        &mut self,
        draft: Draft,
        ask: AskContext,
        tools: ToolRegistry,
        external: Option<ExternalAccess>,
        settings: &Settings,
        ctx: &egui::Context,
    ) {
        if draft.is_empty() {
            return;
        }
        if self.run.is_some() {
            self.queue.push(draft);
            return;
        }
        let (text, spans) = draft.prompt();
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
        for attachment in draft.images() {
            match attachment.image {
                AttachedImage::Region(image) => message.attach_image(image),
                AttachedImage::Picture(image) => message.attach_user_image(image),
            }
            thumbnails.push(attachment.thumbnail);
        }
        let mentions = draft.mentions();
        debug_assert_eq!(spans.len(), draft.chips.len());
        self.messages.push(message);
        self.views.push(MessageView {
            draft: Some(draft),
            images: thumbnails,
            sent_at: Some(current_timestamp()),
            ..MessageView::default()
        });

        if let Some(external) = external {
            self.start_external(external, mentions, settings, ctx);
            return;
        }
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
        let mut question_message = self.messages[question].clone();
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
        self.views.push(MessageView::default());
        self.answerer = config.model.clone();
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
        self.run_context = tool_context.clone();
        let cancel = AgentCancellationToken::default();
        let (events_tx, events) = mpsc::channel();
        let (worker_cancel, ctx) = (cancel.clone(), ctx.clone());
        threads::spawn(move || {
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
            // Mentions are read here, off the UI thread.
            if !mentions.is_empty() {
                question_message = resolve_mentions(question_message, &mentions, &worker_cancel);
                let _ = events_tx.send(RunEvent::Prepared(question_message.clone()));
            }
            history.extend(question_message.into_agent_messages());
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
            started: Instant::now(),
        });
    }

    /// Runs the turn on an external agent: its process reads the message
    /// (with the reader state and mentions) and reaches the Reader through
    /// the MCP server; its working folder is checkpointed around the turn.
    fn start_external(
        &mut self,
        external: ExternalAccess,
        mentions: Vec<Mention>,
        settings: &Settings,
        ctx: &egui::Context,
    ) {
        let question = self.messages.len() - 1;
        let question_message = self.messages[question].clone();
        if self.title.is_none() && question == 0 {
            let mut config = settings.ai.resolved_main();
            config.thinking_level = AiThinkingLevel::Off;
            self.title_config = settings
                .ai
                .resolved_title()
                .or_else(|| config.can_request().then_some(config));
        }
        self.messages.push(ChatMessage::assistant(String::new()));
        self.views.push(MessageView::default());
        self.answerer = external.agent.name.clone();
        self.run_context = self.tool_context(question, Some(&external.agent.name));
        let resume = self.agent_session.clone();
        let cancel = AgentCancellationToken::default();
        let (events_tx, events) = mpsc::channel();
        let (worker_cancel, ctx) = (cancel.clone(), ctx.clone());
        threads::spawn(move || {
            let send = |event: RunEvent| {
                let _ = events_tx.send(event);
                ctx.request_repaint();
            };
            let mut question_message = question_message;
            if !mentions.is_empty() {
                question_message = resolve_mentions(question_message, &mentions, &worker_cancel);
                send(RunEvent::Prepared(question_message.clone()));
            }
            let images = question_message
                .into_agent_messages()
                .into_iter()
                .flat_map(|message| message.content)
                .filter_map(|part| match part {
                    AgentContentPart::Image(image) => Some(image),
                    _ => None,
                })
                .collect();
            let turn = ExternalTurn {
                agent: external.agent,
                scratch: external.scratch.clone(),
                mcp: external.mcp,
                resume,
                model: external.model,
                prompt: question_message.request_text(),
                images,
            };
            let before = turn.scratch.checkpoint().ok();
            let mut answer = ExternalAnswer::default();
            let result =
                glyphst_agent_host::external::run_turn(&turn, &worker_cancel, &mut |event| {
                    answer.add(&event);
                    send(RunEvent::External(event));
                });
            if let (Some(before), Ok(after)) = (before, turn.scratch.checkpoint())
                && let Ok(changes) = turn.scratch.changes(&before, &after)
                && !changes.files.is_empty()
            {
                send(RunEvent::Files(turn.scratch.clone(), changes));
            }
            send(match result {
                Ok(()) => RunEvent::Finished(answer.trace()),
                Err(error) => RunEvent::Error(format!("{error:#}")),
            });
        });
        self.run = Some(Run {
            cancel,
            events,
            answer: self.messages.len() - 1,
            started: Instant::now(),
        });
    }

    /// The tool context of a turn answering the message at `turn`.
    pub fn tool_context(&mut self, turn: usize, agent: Option<&str>) -> ToolContext {
        if self.chat_id == 0 {
            self.chat_id = next_chat_id();
        }
        ToolContext {
            thread: Some(self.chat_id),
            turn,
            origin: agent.map_or(ToolOrigin::Builtin, |agent| {
                ToolOrigin::External(agent.to_string())
            }),
            ..ToolContext::default()
        }
    }

    /// The index the next user message gets.
    pub fn next_turn(&self) -> usize {
        self.messages.len()
    }

    /// Files external agents changed since the last call.
    pub fn take_file_changes(&mut self) -> Vec<(ToolContext, Scratch, FileChanges)> {
        std::mem::take(&mut self.file_changes)
    }

    /// The next queued message, once nothing runs.
    pub fn take_queued(&mut self) -> Option<Draft> {
        (self.run.is_none() && self.error.is_none() && !self.queue.is_empty())
            .then(|| self.queue.remove(0))
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
        let (answer, started) = (run.answer, run.started);
        let events: Vec<RunEvent> = run.events.try_iter().collect();
        let mut finished = false;
        for event in events {
            match event {
                RunEvent::Agent(event) => self.apply(answer, event),
                RunEvent::Prepared(message) => self.messages[answer - 1] = message,
                RunEvent::External(event) => self.apply_external(answer, event),
                RunEvent::Files(scratch, changes) => {
                    self.file_changes
                        .push((self.run_context.clone(), scratch, changes));
                }
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
            let view = &mut self.views[answer];
            view.duration_ms = Some(started.elapsed().as_millis() as u64);
            for segment in &mut view.segments {
                if let Segment::Tool(row) = segment
                    && row.state == ToolState::Running
                {
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
        threads::spawn(move || {
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

    fn apply_external(&mut self, answer: usize, event: ExternalEvent) {
        let view = &mut self.views[answer];
        match event {
            ExternalEvent::Session(session) => self.agent_session = Some(session),
            ExternalEvent::Text(text) => {
                self.messages[answer].push_text(&text);
                view.push_text(&text);
            }
            ExternalEvent::Reasoning(text) => view.push_reasoning(&text),
            ExternalEvent::ToolStarted { id, title } => {
                view.segments.push(Segment::Tool(ToolRow {
                    id,
                    name: title,
                    arguments: String::new(),
                    state: ToolState::Running,
                    output: None,
                }));
            }
            ExternalEvent::ToolFinished { id, failed } => {
                if let Some(row) = view.tool_mut(&id) {
                    row.state = if failed {
                        ToolState::Failed
                    } else {
                        ToolState::Done
                    };
                }
            }
            ExternalEvent::Refused(what) => view.segments.push(Segment::Tool(ToolRow {
                id: String::new(),
                name: format!("Refused {what} (outside its folder)"),
                arguments: String::new(),
                state: ToolState::Failed,
                output: None,
            })),
        }
    }

    fn apply(&mut self, answer: usize, event: AgentEvent) {
        let view = &mut self.views[answer];
        if !matches!(event, AgentEvent::Retrying { .. }) {
            self.status = None;
        }
        match event {
            AgentEvent::TextDelta(delta) => {
                self.messages[answer].push_text(&delta);
                view.push_text(&delta);
            }
            AgentEvent::ReasoningDelta(delta) => view.push_reasoning(&delta),
            AgentEvent::Retrying { attempt, delay, .. } => {
                self.status = Some(format!(
                    "The provider is busy — retrying in {:.0}s (attempt {attempt})",
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
            AgentEvent::ToolCallStarted { id, name } => {
                view.segments.push(Segment::Tool(ToolRow {
                    id,
                    name,
                    arguments: String::new(),
                    state: ToolState::Running,
                    output: None,
                }));
            }
            AgentEvent::ToolCallArgumentsDelta { id, delta } => {
                if let Some(row) = view.tool_mut(&id) {
                    row.arguments.push_str(&delta);
                }
            }
            AgentEvent::ToolCallCompleted(call) => {
                if let Some(row) = view.tool_mut(&call.id) {
                    row.arguments = call.arguments;
                }
            }
            AgentEvent::ToolExecutionCompleted(result) => {
                if let Some(row) = view.tool_mut(&result.call_id) {
                    row.state = if result.is_error {
                        ToolState::Failed
                    } else {
                        ToolState::Done
                    };
                    row.output = Some(result_text(&result.content));
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
        self.views.clear();
        self.queue.clear();
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
        self.editing = None;
        self.agent_session = None;
        self.error = None;
        self.show_sessions = false;
    }

    /// Names this conversation in the change log.
    pub fn chat_id(&self) -> u64 {
        self.chat_id
    }

    /// Rewinds the chat to before the user message at `index`; returns the
    /// message as typed, for the composer.
    pub fn rewind(&mut self, index: usize) -> Option<Draft> {
        let message = self.messages.get(index)?;
        if message.role != Role::User {
            return None;
        }
        let draft = self.views[index]
            .draft
            .clone()
            .unwrap_or_else(|| Draft::plain(message.display_text()));
        self.stop();
        self.run = None;
        self.status = None;
        self.error = None;
        self.confirm_restore = None;
        self.editing = None;
        self.compact_next = false;
        self.messages.truncate(index);
        self.views.truncate(index);
        if self
            .summary
            .as_ref()
            .is_some_and(|summary| summary.until > index)
        {
            self.summary = None;
        }
        Some(draft)
    }

    /// Puts `draft` in the composer.
    pub fn compose(&mut self, ctx: &egui::Context, draft: Draft) {
        self.editor.set(ctx, draft);
        ctx.memory_mut(|memory| memory.request_focus(self.editor.id));
    }

    /// Attaches `attachment` to the next message as a chip.
    pub fn attach(&mut self, ctx: &egui::Context, attachment: Attachment) {
        self.editor
            .insert_chip(ctx, None, ChipKind::Image(attachment));
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
        self.views = std::mem::take(&mut other.views);
        self.cache_key = other.cache_key.take();
        self.summary = other.summary.take();
        self.context_tokens = std::mem::take(&mut other.context_tokens);
        self.compact_next = other.compact_next;
        self.chat_id = other.chat_id;
        self.answerer = std::mem::take(&mut other.answerer);
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
        self.agent = session.agent.as_ref().map(|agent| agent.agent.clone());
        self.agent_session = session.agent.and_then(|agent| agent.session);
        for message in session.messages {
            let (chat, view) = chat_message(message, library);
            self.messages.push(chat);
            self.views.push(view);
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
            .zip(&self.views)
            .map(|(message, view)| session_message(message, view, library))
            .collect();
        let session = AiSession {
            id: self.session_id,
            title,
            messages,
            queued_messages: Vec::new(),
            created: self.created,
            updated: now,
            summary: self.summary.clone(),
            agent: self.agent.clone().map(|agent| AiSessionAgent {
                agent,
                session: self.agent_session.clone(),
            }),
        };
        if let Err(error) = library.save_ai_session(session) {
            self.error = Some(format!("Could not save the chat: {error:#}"));
        }
        self.refresh_sessions(library);
    }

    /// Attaches a pasted or dropped picture to the next message.
    pub fn attach_picture(
        &mut self,
        ctx: &egui::Context,
        name: Option<String>,
        bytes: Vec<u8>,
    ) -> Result<(), String> {
        let attachment = picture_attachment(ctx, name, bytes)?;
        self.attach(ctx, attachment);
        Ok(())
    }

    /// Reads a picture from the clipboard (egui pastes only text), on a
    /// worker thread; [`Self::poll_paste`] attaches it.
    fn start_paste(&mut self, ctx: &egui::Context) {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        threads::spawn(move || {
            if let Some(bytes) = clipboard_picture() {
                let _ = sender.send(bytes);
                ctx.request_repaint();
            }
        });
        self.paste_job = Some(receiver);
    }

    fn poll_paste(&mut self, ctx: &egui::Context) {
        if let Some(job) = &self.paste_job
            && let Ok(bytes) = job.try_recv()
        {
            self.paste_job = None;
            if let Err(error) = self.attach_picture(ctx, None, bytes) {
                self.error = Some(format!("Could not attach the picture: {error}"));
            }
        }
    }

    /// The panel.
    pub fn ui(&mut self, ui: &mut egui::Ui, panel: Panel<'_>) -> Option<PanelAction> {
        self.previews.poll(ui.ctx());
        let mut action = self.header_ui(ui, &panel);
        // Under the full-width header, the body keeps the pane's padding.
        let body = ui
            .available_rect_before_wrap()
            .shrink2(egui::vec2(12.0, 0.0))
            .with_max_y(ui.max_rect().bottom() - 12.0);
        let mut body_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(body)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        body_ui.set_min_size(body.size());
        let ui = &mut body_ui;
        if self.show_sessions {
            if let Some(chosen) = self.history_ui(ui) {
                action = Some(chosen);
            }
            return action;
        }
        let Panel {
            document,
            page,
            selection_words,
            models,
            model,
            changes,
            sources,
            agents,
        } = panel;
        let composer = composer::Composer {
            document: document.as_deref(),
            page,
            selection_words,
            models,
            model,
            agents,
        };
        if let Some(chosen) = self.body_ui(ui, &composer, changes, sources) {
            action = Some(chosen);
        }
        self.previews.show_hover(ui.ctx());
        action
    }
}

/// An empty saved message, standing for one in canned histories.
fn session_message_stub() -> AiSessionMessage {
    AiSessionMessage {
        role: AiSessionRole::User,
        content: String::new(),
        request_content: None,
        references: Vec::new(),
        occurrences: Vec::new(),
        images: Vec::new(),
        agent_parts: Vec::new(),
        document_path: None,
        document_label: None,
        mentions: Vec::new(),
        sent_at: None,
        duration_ms: None,
    }
}

/// A picture as an attachment, with its thumbnail.
pub fn picture_attachment(
    ctx: &egui::Context,
    name: Option<String>,
    bytes: Vec<u8>,
) -> Result<Attachment, String> {
    if bytes.len() > MAX_PICTURE_BYTES {
        return Err("the picture is larger than 16 MB".into());
    }
    let reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let media_type = match reader.format() {
        Some(image::ImageFormat::Png) => "image/png",
        Some(image::ImageFormat::Jpeg) => "image/jpeg",
        Some(image::ImageFormat::Gif) => "image/gif",
        Some(image::ImageFormat::WebP) => "image/webp",
        _ => return Err("only PNG, JPEG, GIF and WebP pictures can be attached".into()),
    };
    let decoded = reader.decode().map_err(|error| error.to_string())?;
    let (width, height) = (decoded.width(), decoded.height());
    let thumbnail = thumbnail_texture(ctx, &decoded);
    let label = name.clone().unwrap_or_else(|| "Picture".into());
    Ok(Attachment {
        image: AttachedImage::Picture(
            AgentImage::new(media_type, width, height, bytes).with_optional_name(name),
        ),
        thumbnail,
        label,
    })
}

fn thumbnail_texture(ctx: &egui::Context, image: &image::DynamicImage) -> egui::TextureHandle {
    let small = image
        .thumbnail(THUMBNAIL_PIXELS, THUMBNAIL_PIXELS)
        .to_rgba8();
    let size = [small.width() as usize, small.height() as usize];
    ctx.load_texture(
        format!("assistant-picture-{}", unique_suffix()),
        egui::ColorImage::from_rgba_unmultiplied(size, small.as_raw()),
        egui::TextureOptions::LINEAR,
    )
}

/// The text of a tool's result, for its details.
fn result_text(content: &[AgentToolResultPart]) -> String {
    content
        .iter()
        .map(|part| match part {
            AgentToolResultPart::Text(text) => text.clone(),
            AgentToolResultPart::Image(_) => "[image]".into(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A chat message as the library stores it: its text and chips, what was
/// sent with its images, and an answer's whole trace (reasoning, tool calls
/// and results with their images, and the provider's reasoning state).
fn session_message(
    message: &ChatMessage,
    view: &MessageView,
    library: &mut LibraryStore,
) -> AiSessionMessage {
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
    let mentions = view
        .draft
        .as_ref()
        .map(|draft| {
            let (_, spans) = draft.prompt();
            spans
                .into_iter()
                .zip(&draft.chips)
                .map(|(span, chip)| {
                    let (kind, text) = match &chip.kind {
                        ChipKind::Mention(mention) => mention.saved_kind(),
                        ChipKind::Image(_) => ("image", None),
                    };
                    AiSessionMention {
                        start: span.start,
                        end: span.end,
                        label: chip.kind.label(),
                        kind: kind.to_string(),
                        text,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
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
        mentions,
        sent_at: view.sent_at,
        duration_ms: view.duration_ms,
    }
}

/// A picture on the clipboard, if it holds one: `wl-paste` on Wayland,
/// `xclip` on X11.
fn clipboard_picture() -> Option<Vec<u8>> {
    let run = |program: &str, arguments: &[&str]| {
        std::process::Command::new(program)
            .args(arguments)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| output.stdout)
    };
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let types = if wayland {
        run("wl-paste", &["--list-types"])?
    } else {
        run("xclip", &["-selection", "clipboard", "-t", "TARGETS", "-o"])?
    };
    let types = String::from_utf8_lossy(&types);
    let kind = ["image/png", "image/jpeg", "image/webp", "image/gif"]
        .into_iter()
        .find(|kind| types.lines().any(|line| line.trim() == *kind))?;
    let bytes = if wayland {
        run("wl-paste", &["--no-newline", "--type", kind])?
    } else {
        run("xclip", &["-selection", "clipboard", "-t", kind, "-o"])?
    };
    (!bytes.is_empty()).then_some(bytes)
}

/// `message` with its mentions read: their context block and images.
fn resolve_mentions(
    mut message: ChatMessage,
    mentions: &[Mention],
    cancel: &AgentCancellationToken,
) -> ChatMessage {
    let mut blocks = Vec::new();
    for mention in mentions {
        let (block, images) = mention.resolve(cancel);
        blocks.push(block);
        for image in images {
            message.attach_user_image(image);
        }
    }
    if let Some(context) = ai_mentions::context(&blocks) {
        message = message.with_mention_context(context);
    }
    message
}

/// An external agent's answer as the chat saves it: its reasoning, text
/// and tool lines.
#[derive(Default)]
struct ExternalAnswer {
    reasoning: String,
    text: String,
    tools: Vec<(String, String, Option<bool>)>,
}

impl ExternalAnswer {
    fn add(&mut self, event: &ExternalEvent) {
        match event {
            ExternalEvent::Text(text) => self.text.push_str(text),
            ExternalEvent::Reasoning(text) => self.reasoning.push_str(text),
            ExternalEvent::ToolStarted { id, title } => {
                self.tools.push((id.clone(), title.clone(), None));
            }
            ExternalEvent::ToolFinished { id, failed } => {
                if let Some(tool) = self.tools.iter_mut().find(|tool| &tool.0 == id) {
                    tool.2 = Some(*failed);
                }
            }
            ExternalEvent::Session(_) | ExternalEvent::Refused(_) => {}
        }
    }

    fn trace(self) -> Vec<AgentMessage> {
        let mut content = Vec::new();
        if !self.reasoning.is_empty() {
            content.push(AgentContentPart::Reasoning(self.reasoning));
        }
        content.extend(self.tools.iter().map(|(id, title, _)| {
            AgentContentPart::ToolCall(AgentToolCall {
                id: id.clone(),
                name: title.clone(),
                arguments: "{}".into(),
            })
        }));
        let mut trace = vec![AgentMessage {
            role: AgentRole::Assistant,
            content,
        }];
        trace.extend(self.tools.into_iter().map(|(id, title, failed)| {
            AgentMessage::tool_result(AgentToolResult {
                call_id: id,
                tool_name: title,
                content: Vec::new(),
                is_error: failed != Some(false),
            })
        }));
        trace.push(AgentMessage::assistant_text(self.text));
        trace
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
fn chat_message(message: AiSessionMessage, library: &LibraryStore) -> (ChatMessage, MessageView) {
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
    let mut view = MessageView {
        sent_at: message.sent_at,
        duration_ms: message.duration_ms,
        segments: segments(&message.agent_parts),
        ..MessageView::default()
    };
    if role == Role::User {
        let chips = message
            .mentions
            .iter()
            .filter(|chip| message.content.get(chip.start..chip.end).is_some())
            .map(|chip| {
                (
                    chip.start..chip.end,
                    ChipKind::Mention(Mention::saved(
                        &chip.kind,
                        chip.label.clone(),
                        chip.text.clone(),
                    )),
                )
            })
            .collect();
        view.draft = Some(Draft::from_prompt(&message.content, chips));
    }
    let mut chat = ChatMessage::from_persisted(role, message.content, message.request_content)
        .with_document_scope(scope);
    for image in message.images.iter().filter_map(load) {
        view.pending_images.push(image.clone());
        chat.attach_user_image(image);
    }
    let trace = agent_trace(&message.agent_parts, load);
    if !trace.is_empty() {
        chat.set_agent_trace(trace);
    }
    (chat, view)
}

/// An answer's parts in order, as the panel shows them.
fn segments(parts: &[AiSessionAgentPart]) -> Vec<Segment> {
    let results: std::collections::HashMap<&str, String> = parts
        .iter()
        .filter_map(|part| match part {
            AiSessionAgentPart::ToolResult {
                call_id, content, ..
            } => Some((
                call_id.as_str(),
                content
                    .iter()
                    .map(|part| match part {
                        AiSessionToolResultPart::Text(text) => text.clone(),
                        AiSessionToolResultPart::Image(_) => "[image]".into(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            _ => None,
        })
        .collect();
    let mut view = MessageView::default();
    for part in parts {
        match part {
            AiSessionAgentPart::AssistantText(text) => view.push_text(text),
            AiSessionAgentPart::Reasoning(text) => view.push_reasoning(text),
            AiSessionAgentPart::ToolCall {
                id,
                name,
                arguments,
                status,
            } => view.segments.push(Segment::Tool(ToolRow {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
                state: match status {
                    AiSessionToolStatus::Completed => ToolState::Done,
                    _ => ToolState::Failed,
                },
                output: results.get(id.as_str()).cloned(),
            })),
            AiSessionAgentPart::ToolResult { .. } | AiSessionAgentPart::OpaqueReasoning { .. } => {}
        }
    }
    view.segments
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
        quick.send(
            Draft::plain("First?"),
            ask(),
            ToolRegistry::new(),
            None,
            &settings,
            &ctx,
        );
        quick.new_session();
        quick.send(
            Draft::plain("Second?"),
            ask(),
            ToolRegistry::new(),
            None,
            &settings,
            &ctx,
        );
        assert_eq!(quick.messages().len(), 1, "no history");
        assert_eq!(quick.messages()[0].display_text(), "Second?");

        let mut assistant = Assistant::default();
        assistant.send(
            Draft::plain("Earlier"),
            ask(),
            ToolRegistry::new(),
            None,
            &settings,
            &ctx,
        );
        assistant.send(
            Draft::plain("Later"),
            ask(),
            ToolRegistry::new(),
            None,
            &settings,
            &ctx,
        );
        assert_eq!(assistant.messages().len(), 2);
        assert_eq!(
            assistant.rewind(1).map(|draft| draft.prompt().0).as_deref(),
            Some("Later")
        );
        assert_eq!(assistant.messages().len(), 1);
        assert!(assistant.adopt(&mut quick));
        assert_eq!(assistant.messages().len(), 1, "a new chat of the quick one");
        assert_eq!(assistant.messages()[0].display_text(), "Second?");
        assert!(quick.messages().is_empty());
    }

    /// One frame of the panel with `events`.
    fn panel_frame(ctx: &egui::Context, assistant: &mut Assistant, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(500.0, 700.0),
            )),
            events,
            ..Default::default()
        };
        let mut changes = ChangeLog::default();
        let mut output = ctx.run_ui(input, |ui| {
            let document = crate::ai_tools::OpenDocument {
                id: "doc_1".into(),
                title: "Paper".into(),
                path: "/nonexistent.pdf".into(),
                layout: DocumentLayout::Fixed,
                page_count: 9,
                current_page: 0,
            };
            assistant.ui(
                ui,
                Panel {
                    document: Some("Paper".into()),
                    page: Some(0),
                    selection_words: None,
                    models: &[],
                    model: None,
                    changes: &mut changes,
                    sources: &mut || Sources {
                        documents: vec![document.clone()],
                        ..Sources::default()
                    },
                    agents: &[],
                },
            );
        });
        output.textures_delta.clear();
    }

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn the_composer_menu_attaches_mentions_and_runs_commands() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let mut assistant = Assistant::default();
        ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("assistant-input")));
        panel_frame(&ctx, &mut assistant, Vec::new());
        panel_frame(
            &ctx,
            &mut assistant,
            vec![egui::Event::Text("Explain @page2".into())],
        );
        assert_eq!(assistant.editor.text(), "Explain @page2");
        panel_frame(&ctx, &mut assistant, vec![key(egui::Key::ArrowDown)]);
        panel_frame(&ctx, &mut assistant, vec![key(egui::Key::Enter)]);
        assert_eq!(
            assistant.editor.draft().prompt().0,
            "Explain [Paper p. 2 image]",
            "the query gives way to a chip"
        );
        assert_eq!(assistant.editor.chips().len(), 1);
        assert!(
            matches!(
                &assistant.editor.chips()[0].kind,
                ChipKind::Mention(Mention::PageImages { pages, .. }) if pages == &[1]
            ),
            "Down chose the page image: {:?}",
            assistant.editor.chips()[0].kind
        );

        // While an input method composes, Enter is its own.
        panel_frame(&ctx, &mut assistant, vec![egui::Event::Text("@".into())]);
        let preedit = egui::Event::Ime(egui::ImeEvent::Preedit {
            text: "ぺ".into(),
            active_range_chars: None,
        });
        panel_frame(&ctx, &mut assistant, vec![preedit, key(egui::Key::Enter)]);
        assert_eq!(assistant.editor.chips().len(), 1, "the menu took no key");
        assert!(assistant.messages.is_empty(), "nor did sending");
        let commit = egui::Event::Ime(egui::ImeEvent::Commit("ぺ".into()));
        panel_frame(&ctx, &mut assistant, vec![commit]);

        // Slash commands run from the start of the composer.
        assistant.editor.take();
        assistant
            .messages
            .push(ChatMessage::user_text("Earlier".into()));
        assistant.views.push(MessageView::default());
        panel_frame(
            &ctx,
            &mut assistant,
            vec![egui::Event::Text("/comp".into())],
        );
        panel_frame(&ctx, &mut assistant, vec![key(egui::Key::Enter)]);
        assert!(assistant.compact_next);
        assert_eq!(assistant.editor.text(), "");
    }

    #[test]
    fn backspacing_a_mention_query_never_panics() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let mut assistant = Assistant::default();
        ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("assistant-input")));
        panel_frame(&ctx, &mut assistant, Vec::new());
        let text = |text: &str| egui::Event::Text(text.into());
        for typed in ["@", "p", "a", "g", "e"] {
            panel_frame(&ctx, &mut assistant, vec![text(typed)]);
        }
        assert_eq!(assistant.editor.text(), "@page");
        // Typing and deleting in one frame, then Backspace with Enter.
        panel_frame(
            &ctx,
            &mut assistant,
            vec![text("x"), key(egui::Key::Backspace)],
        );
        panel_frame(
            &ctx,
            &mut assistant,
            vec![key(egui::Key::Backspace), key(egui::Key::Enter)],
        );
        for _ in 0..8 {
            panel_frame(&ctx, &mut assistant, vec![key(egui::Key::Backspace)]);
        }
        assert_eq!(assistant.editor.text(), "");
        assert!(assistant.messages.is_empty(), "Enter sent nothing");

        // A query after other text.
        panel_frame(&ctx, &mut assistant, vec![text("ask @pa")]);
        for _ in 0..4 {
            panel_frame(&ctx, &mut assistant, vec![key(egui::Key::Backspace)]);
        }
        assert_eq!(assistant.editor.text(), "ask");
    }

    #[test]
    fn external_agents_answer_in_the_panel_and_their_files_can_be_undone() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory =
            std::env::temp_dir().join(format!("glyphst-reader-external-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        // A stand-in for `claude` speaking its stream-json protocol.
        let program = directory.join("claude");
        std::fs::write(
            &program,
            r#"#!/bin/sh
read line
echo '{"type":"system","subtype":"init","session_id":"s1"}'
echo '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"notes.md"}}]}}'
echo hello > notes.md
echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":false}]}}'
echo '{"type":"stream_event","parent_tool_use_id":null,"event":{"delta":{"type":"text_delta","text":"Done."}}}'
echo '{"type":"result","subtype":"success","is_error":false,"session_id":"s1"}'
cat > /dev/null
"#,
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let scratch = Scratch::open(&directory, "doc", "Doc").unwrap();
        let mut library = LibraryStore::open_for_test(directory.join("library.redb")).unwrap();
        let ctx = egui::Context::default();
        let mut assistant = Assistant {
            agent: Some("claude-code".into()),
            ..Assistant::default()
        };
        let ask = AskContext {
            path: directory.join("doc.pdf"),
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
        let external = ExternalAccess {
            agent: AgentSpec::known(glyphst_agent_host::external::AgentKind::ClaudeCode, program),
            scratch: scratch.clone(),
            mcp: McpAccess {
                url: "http://127.0.0.1:9/mcp".into(),
                token: "token".into(),
            },
            model: None,
        };
        assistant.send(
            Draft::plain("Write notes"),
            ask,
            ToolRegistry::new(),
            Some(external),
            &Settings::default(),
            &ctx,
        );
        let started = Instant::now();
        while assistant.is_running() {
            assert!(started.elapsed() < Duration::from_secs(30), "timed out");
            assistant.poll(Some(&mut library));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(assistant.error, None);
        assert_eq!(assistant.messages()[1].display_text(), "Done.");
        assert_eq!(assistant.tools(1)[0].label(), "Write notes.md");
        assert_eq!(assistant.tools(1)[0].state, ToolState::Done);
        assert_eq!(assistant.agent_session.as_deref(), Some("s1"));

        let changes = assistant.take_file_changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].2.files, ["notes.md | +1 -0"]);
        assert_eq!(
            changes[0].0.origin,
            ToolOrigin::External("Claude Code".into())
        );
        let mut log = ChangeLog::default();
        let (context, folder, files) = changes.into_iter().next().unwrap();
        log.record_files(&context, folder, files);
        assert_eq!(log.undo_where(&mut library, |_| true), Vec::<String>::new());
        assert!(!scratch.dir.join("notes.md").exists(), "undone");

        // The chat reopens on the same agent and session.
        let mut reopened = Assistant::default();
        reopened.refresh_sessions(&library);
        let id = reopened.sessions[0].id;
        reopened.load_session(id, &library);
        assert_eq!(reopened.agent.as_deref(), Some("claude-code"));
        assert_eq!(reopened.agent_session.as_deref(), Some("s1"));
        assert_eq!(reopened.tools(1)[0].label(), "Write notes.md");
        drop(library);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn pasted_pictures_are_attached_with_their_size() {
        let ctx = egui::Context::default();
        let mut assistant = Assistant::default();
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(300, 120, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assistant
            .attach_picture(&ctx, Some("chart.png".into()), png)
            .unwrap();
        let ChipKind::Image(attachment) = &assistant.editor.chips()[0].kind else {
            panic!("a picture chip");
        };
        match &attachment.image {
            AttachedImage::Picture(image) => {
                assert_eq!((image.width, image.height), (300, 120));
                assert_eq!(image.media_type, "image/png");
                assert_eq!(image.name.as_deref(), Some("chart.png"));
            }
            AttachedImage::Region(_) => panic!("a picture"),
        }
        assert_eq!(attachment.thumbnail.size(), [160, 64]);
        assert!(
            assistant
                .attach_picture(&ctx, None, b"not a picture".to_vec())
                .is_err()
        );
        assert_eq!(assistant.editor.chips().len(), 1);
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
        assistant.attach(
            &ctx,
            Attachment {
                image: AttachedImage::Region(ImageContext::new_region(
                    reference,
                    "image/png",
                    vec![1, 2, 3],
                )),
                thumbnail,
                label: "Region of page 1".into(),
            },
        );
        let mut draft = assistant.editor.take();
        draft.text.push_str("What is this?");
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
        assistant.send(draft, ask(), ToolRegistry::new(), None, &settings, &ctx);
        let images = |message: &ChatMessage| {
            message
                .into_agent_messages()
                .iter()
                .flat_map(|message| &message.content)
                .filter(|part| matches!(part, AgentContentPart::Image(_)))
                .count()
        };
        assert_eq!(images(&assistant.messages()[0]), 1);
        assert!(assistant.editor.chips().is_empty());
        assert_eq!(assistant.views[0].images.len(), 1);
        assistant.send(
            Draft::plain("And now?"),
            ask(),
            ToolRegistry::new(),
            None,
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
        assistant.editor.insert_chip(
            &ctx,
            None,
            ChipKind::Mention(Mention::Pages {
                document: crate::ai_tools::OpenDocument {
                    id: "doc_1".into(),
                    title: "doc.pdf".into(),
                    path: path.clone(),
                    layout: DocumentLayout::Fixed,
                    page_count: 1,
                    current_page: 0,
                },
                first: 0,
                last: 0,
            }),
        );
        let mut draft = assistant.editor.take();
        draft.text.push_str("Where does the story end?");
        assistant.send(draft, ask, tools, None, &settings, &ctx);
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
        assert_eq!(rows[0].label(), format!("Searched “{FAKE_SEARCH}”"));
        assert_eq!(rows[0].state, ToolState::Done);
        assert!(
            assistant.messages()[0]
                .request_text()
                .contains("Here the story begins."),
            "the page goes with the question"
        );
        let request = assistant.messages()[0].request_text();
        assert!(
            request.contains("<mention kind=\"pages\" document=\"doc_1\"")
                && request.contains("And the story ends."),
            "the mentioned page is read when sent: {request}"
        );
        let sent = assistant.views[0].draft.as_ref().unwrap();
        assert_eq!(sent.prompt().0, "[doc.pdf p. 1] Where does the story end?");
        assert!(assistant.views[1].duration_ms.is_some());

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
        let chips = &reopened.views[0].draft.as_ref().unwrap().chips;
        assert_eq!(chips.len(), 1, "the chip comes back");
        assert_eq!(chips[0].kind.label(), "doc.pdf p. 1");
        assert_eq!(reopened.views[0].sent_at, assistant.views[0].sent_at);
        assert_eq!(
            reopened.views[1].segments, assistant.views[1].segments,
            "the answer's parts come back in order"
        );
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
            Draft::plain("And then?"),
            ask,
            ToolRegistry::new(),
            None,
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
        unconfigured.send(
            Draft::plain("Hello?"),
            ask,
            ToolRegistry::new(),
            None,
            &missing,
            &ctx,
        );
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
