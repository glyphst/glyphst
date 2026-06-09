use std::{collections::HashSet, io::Read as _, ops::Range, path::PathBuf, thread};

use crate::{
    library::{AiSession, AiSessionMessage, AiSessionRole, LibraryStore, current_timestamp},
    pdf_view::PdfView,
    settings::{AiProvider, AiSettings, SettingsStore},
    settings_panel::PanelEvent,
};
use anyhow::{Context as _, Result, anyhow};
use futures::{StreamExt, channel::mpsc};
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, Pixels, Point as GpuiPoint, Render, ScrollHandle, SharedString, Style,
    Task, TextRun, UTF16Selection, WeakEntity, Window, WrappedLine, div, fill, point, prelude::*,
    px, relative, rgb, rgba, size,
};
use serde_json::{Value, json};

gpui::actions!(
    ai_panel,
    [
        AiPanelClose,
        AiPanelSubmit,
        AiPanelNewline,
        AiPanelBackspace,
        AiPanelDelete,
        AiPanelLeft,
        AiPanelRight,
        AiPanelUp,
        AiPanelDown,
        AiPanelHome,
        AiPanelEnd,
        AiPanelSelectAll,
        AiPanelSelectLeft,
        AiPanelSelectRight,
        AiPanelSelectUp,
        AiPanelSelectDown,
        AiPanelSelectHome,
        AiPanelSelectEnd,
        AiPanelCopy,
        AiPanelCut,
        AiPanelPaste
    ]
);

const AI_PANEL_WIDTH: f32 = 320.0;
const COMPLETION_MENU_WIDTH: f32 = 248.0;
const COMPLETION_MENU_ROW_HEIGHT: f32 = 46.0;
const MAX_CONTEXT_CHARS: usize = 12_000;
const DEFAULT_SESSION_TITLE: &str = "New Chat";
const TITLE_GENERATION_PROMPT: &str = "Generate a concise 3-7 word title for this conversation, \
omitting punctuation. Go straight to the title, without any preamble or prefix like `Title:`. \
If the conversation is about a specific subject, include it in the title. Be descriptive. \
Do not speak in the first person.";

#[derive(Clone, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
}

#[derive(Clone)]
struct ChatMessage {
    role: Role,
    content: String,
    request_content: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum MentionKind {
    CurrentPageText,
    Selection,
}

impl MentionKind {
    const ALL: [Self; 2] = [Self::CurrentPageText, Self::Selection];

    fn token(self) -> &'static str {
        match self {
            Self::CurrentPageText => "@current page text",
            Self::Selection => "@selection",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::CurrentPageText => "Current page text",
            Self::Selection => "Selection",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::CurrentPageText => "Attach text from the visible page",
            Self::Selection => "Attach the selected PDF text",
        }
    }

    fn context_title(self) -> &'static str {
        match self {
            Self::CurrentPageText => "Current page text",
            Self::Selection => "Selection",
        }
    }
}

#[derive(Clone)]
struct MentionCompletion {
    source_range: Range<usize>,
    selected_ix: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AiPanelView {
    Chat,
    Sessions,
}

enum AiStreamEvent {
    Delta(String),
    Error(String),
    Done,
}

pub struct AiPanel {
    focus_handle: FocusHandle,
    previous_focus_handle: FocusHandle,
    messages: Vec<ChatMessage>,
    content: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_lines: Vec<LaidOutLine>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    loading: bool,
    scroll_handle: ScrollHandle,
    pending_stream: Option<Task<()>>,
    pending_title_task: Option<Task<()>>,
    pdf_view: WeakEntity<PdfView>,
    document_path: PathBuf,
    current_page: usize,
    session_id: u64,
    session_title: String,
    session_created: u64,
    session_updated: u64,
    sessions: Vec<AiSession>,
    view: AiPanelView,
    mention_completion: Option<MentionCompletion>,
    last_cursor_pos: Option<GpuiPoint<Pixels>>,
}

struct LaidOutLine {
    start: usize,
    line: WrappedLine,
}

struct AiInputElement {
    panel: Entity<AiPanel>,
}

struct PrepaintState {
    lines: Option<Vec<LaidOutLine>>,
    cursor: Option<PaintQuad>,
    cursor_position: Option<GpuiPoint<Pixels>>,
    selections: Vec<PaintQuad>,
}

impl AiPanel {
    pub fn new(
        previous_focus_handle: FocusHandle,
        pdf_view: WeakEntity<PdfView>,
        document_path: PathBuf,
        current_page: usize,
        cx: &mut Context<Self>,
    ) -> Self {
        let session_id = cx.global_mut::<LibraryStore>().next_ai_session_id();
        let sessions = cx.global::<LibraryStore>().ai_sessions_for(&document_path);
        Self {
            focus_handle: cx.focus_handle(),
            previous_focus_handle,
            messages: Vec::new(),
            content: SharedString::default(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_lines: Vec::new(),
            last_bounds: None,
            is_selecting: false,
            loading: false,
            scroll_handle: ScrollHandle::new(),
            pending_stream: None,
            pending_title_task: None,
            pdf_view,
            document_path,
            current_page,
            session_id,
            session_title: DEFAULT_SESSION_TITLE.to_string(),
            session_created: 0,
            session_updated: 0,
            sessions,
            view: AiPanelView::Chat,
            mention_completion: None,
            last_cursor_pos: None,
        }
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_current_page(cx);
        self.save_current_session(cx);
        self.previous_focus_handle.focus(window, cx);
        cx.emit(PanelEvent::Dismissed);
    }

    fn close(&mut self, _: &AiPanelClose, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(window, cx);
    }

    fn submit(&mut self, _: &AiPanelSubmit, _: &mut Window, cx: &mut Context<Self>) {
        self.send_message(cx);
    }

    fn newline(&mut self, _: &AiPanelNewline, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_mention_completion(cx) {
            return;
        }
        self.replace_text_in_range(None, "\n", window, cx);
    }

    fn backspace(&mut self, _: &AiPanelBackspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &AiPanelDelete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn left(&mut self, _: &AiPanelLeft, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &AiPanelRight, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn up(&mut self, _: &AiPanelUp, window: &mut Window, cx: &mut Context<Self>) {
        if self.select_previous_mention_completion(cx) {
            return;
        }
        self.move_vertically(-1, false, window, cx);
    }

    fn down(&mut self, _: &AiPanelDown, window: &mut Window, cx: &mut Context<Self>) {
        if self.select_next_mention_completion(cx) {
            return;
        }
        self.move_vertically(1, false, window, cx);
    }

    fn home(&mut self, _: &AiPanelHome, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_start_offset(), cx);
    }

    fn end(&mut self, _: &AiPanelEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_end_offset(), cx);
    }

    fn select_all(&mut self, _: &AiPanelSelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn select_left(&mut self, _: &AiPanelSelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &AiPanelSelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_up(&mut self, _: &AiPanelSelectUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(-1, true, window, cx);
    }

    fn select_down(&mut self, _: &AiPanelSelectDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(1, true, window, cx);
    }

    fn select_home(&mut self, _: &AiPanelSelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_start_offset(), cx);
    }

    fn select_end(&mut self, _: &AiPanelSelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_end_offset(), cx);
    }

    fn copy(&mut self, _: &AiPanelCopy, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_selected_text(cx);
    }

    fn cut(&mut self, _: &AiPanelCut, window: &mut Window, cx: &mut Context<Self>) {
        if self.copy_selected_text(cx) {
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn paste(&mut self, _: &AiPanelPaste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn send_message(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }

        let prompt = self.content.trim().to_string();
        if prompt.is_empty() {
            return;
        }

        let settings = cx.global::<SettingsStore>().settings.ai.clone();
        self.update_current_page(cx);
        let request_content = self.request_content_for_prompt(&prompt, cx);
        if self.messages.is_empty() {
            self.session_title = session_title_for_prompt(&prompt);
        }
        self.content = SharedString::default();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.mention_completion = None;
        self.last_cursor_pos = None;
        self.messages.push(ChatMessage {
            role: Role::User,
            content: prompt,
            request_content: Some(request_content),
        });
        self.messages.push(ChatMessage {
            role: Role::Assistant,
            content: String::new(),
            request_content: None,
        });
        self.loading = true;
        self.save_current_session(cx);
        cx.notify();

        let request_messages = self.messages[..self.messages.len().saturating_sub(1)].to_vec();
        let (tx, mut rx) = mpsc::unbounded::<AiStreamEvent>();

        thread::spawn(move || {
            let mut send_event = |event| tx.unbounded_send(event).map_err(|_| anyhow!("closed"));
            let result = stream_completion(settings, request_messages, &mut send_event);
            match result {
                Ok(()) => {
                    let _ = send_event(AiStreamEvent::Done);
                }
                Err(error) if error.to_string() != "closed" => {
                    let _ = send_event(AiStreamEvent::Error(format!("{error:#}")));
                }
                Err(_) => {}
            }
        });

        self.pending_stream = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                if this
                    .update(cx, |this, cx| {
                        this.apply_stream_event(event, cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn apply_stream_event(&mut self, event: AiStreamEvent, cx: &mut Context<Self>) {
        match event {
            AiStreamEvent::Delta(delta) => {
                if let Some(message) = self.messages.last_mut() {
                    message.content.push_str(&delta);
                }
            }
            AiStreamEvent::Error(error) => {
                self.loading = false;
                if let Some(message) = self.messages.last_mut() {
                    if !message.content.is_empty() {
                        message.content.push_str("\n\n");
                    }
                    message.content.push_str("Error: ");
                    message.content.push_str(&error);
                }
                self.pending_stream = None;
                self.save_current_session(cx);
            }
            AiStreamEvent::Done => {
                self.loading = false;
                self.pending_stream = None;
                self.maybe_generate_title(cx);
                self.save_current_session(cx);
            }
        }
    }

    fn maybe_generate_title(&mut self, cx: &mut Context<Self>) {
        if self.pending_title_task.is_some() || !self.is_first_turn() {
            return;
        }

        let Some(title_settings) = cx.global::<SettingsStore>().settings.ai_title_model.clone()
        else {
            return;
        };
        if !title_settings.can_request() {
            return;
        }

        let session_id = self.session_id;
        let mut title_messages = self.messages.clone();
        title_messages.push(ChatMessage {
            role: Role::User,
            content: TITLE_GENERATION_PROMPT.to_string(),
            request_content: None,
        });

        let settings: AiSettings = title_settings.into();
        let (tx, mut rx) = mpsc::unbounded::<AiStreamEvent>();

        thread::spawn(move || {
            let mut send_event = |event| tx.unbounded_send(event).map_err(|_| anyhow!("closed"));
            let result = stream_completion(settings, title_messages, &mut send_event);
            match result {
                Ok(()) => {
                    let _ = send_event(AiStreamEvent::Done);
                }
                Err(error) if error.to_string() != "closed" => {
                    let _ = send_event(AiStreamEvent::Error(format!("{error:#}")));
                }
                Err(_) => {}
            }
        });

        let mut title_response = String::new();
        self.pending_title_task = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                match event {
                    AiStreamEvent::Delta(delta) => title_response.push_str(&delta),
                    AiStreamEvent::Done => {
                        if let Some(title) = title_from_response(&title_response) {
                            let _ = this.update(cx, |this, cx| {
                                if this.session_id == session_id {
                                    this.session_title = title;
                                    this.save_current_session(cx);
                                    cx.notify();
                                }
                            });
                        }
                        break;
                    }
                    AiStreamEvent::Error(_) => break,
                }
            }

            let _ = this.update(cx, |this, _| {
                if this.session_id == session_id {
                    this.pending_title_task = None;
                }
            });
        }));
    }

    fn is_first_turn(&self) -> bool {
        self.messages.len() == 2
            && self.messages[0].role == Role::User
            && self.messages[1].role == Role::Assistant
    }

    fn request_content_for_prompt(&self, prompt: &str, cx: &mut Context<Self>) -> String {
        let mentions = mentioned_kinds(prompt);
        if mentions.is_empty() {
            return prompt.to_string();
        }

        let mut request = prompt.to_string();
        request.push_str("\n\nAttached PDF context from @ mentions:");
        for mention in mentions {
            let context = self.resolve_mention(mention, cx);
            request.push_str("\n\n### ");
            request.push_str(mention.context_title());
            request.push('\n');
            request.push_str(&context);
        }
        request
    }

    fn resolve_mention(&self, mention: MentionKind, cx: &mut Context<Self>) -> String {
        let resolved = self
            .pdf_view
            .update(cx, |pdf_view, _| match mention {
                MentionKind::CurrentPageText => pdf_view.current_page_text().map(|text| {
                    let page = pdf_view.current_page();
                    format!(
                        "Page {page}:\n{}",
                        truncate_context(text).unwrap_or_else(|| {
                            "No text is available on the current page.".to_string()
                        })
                    )
                }),
                MentionKind::Selection => pdf_view.selected_text().map(|text| {
                    truncate_context(text)
                        .unwrap_or_else(|| "No selected text is currently available.".to_string())
                }),
            })
            .ok()
            .flatten();

        resolved.unwrap_or_else(|| match mention {
            MentionKind::CurrentPageText => "No current page text is available.".to_string(),
            MentionKind::Selection => "No selected text is currently available.".to_string(),
        })
    }

    fn update_current_page(&mut self, cx: &mut Context<Self>) {
        if let Ok(page) = self
            .pdf_view
            .update(cx, |pdf_view, _| pdf_view.current_page())
        {
            self.current_page = page;
        }
    }

    pub(crate) fn save_current_session(&mut self, cx: &mut Context<Self>) {
        if self
            .messages
            .iter()
            .all(|message| message.content.trim().is_empty())
        {
            return;
        }

        let now = current_timestamp();
        if self.session_created == 0 {
            self.session_created = now;
        }
        self.session_updated = now;

        let session = AiSession {
            id: self.session_id,
            path: self.document_path.clone(),
            title: self.session_title.clone(),
            page: self.current_page,
            messages: self
                .messages
                .iter()
                .map(|message| AiSessionMessage {
                    role: match message.role {
                        Role::User => AiSessionRole::User,
                        Role::Assistant => AiSessionRole::Assistant,
                    },
                    content: message.content.clone(),
                    request_content: message
                        .request_content
                        .clone()
                        .filter(|content| content != &message.content),
                })
                .collect(),
            created: self.session_created,
            updated: self.session_updated,
        };

        if let Err(error) = cx.global_mut::<LibraryStore>().save_ai_session(session) {
            eprintln!("failed to save AI session: {error:#}");
        }
        self.refresh_sessions(cx);
    }

    fn refresh_sessions(&mut self, cx: &mut Context<Self>) {
        self.sessions = cx
            .global::<LibraryStore>()
            .ai_sessions_for(&self.document_path);
    }

    fn show_sessions(&mut self, cx: &mut Context<Self>) {
        self.update_current_page(cx);
        self.save_current_session(cx);
        self.refresh_sessions(cx);
        self.view = AiPanelView::Sessions;
        cx.notify();
    }

    fn show_chat(&mut self, cx: &mut Context<Self>) {
        self.view = AiPanelView::Chat;
        cx.notify();
    }

    fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.update_current_page(cx);
        self.save_current_session(cx);
        self.session_id = cx.global_mut::<LibraryStore>().next_ai_session_id();
        self.session_title = DEFAULT_SESSION_TITLE.to_string();
        self.session_created = 0;
        self.session_updated = 0;
        self.messages.clear();
        self.content = SharedString::default();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.mention_completion = None;
        self.last_cursor_pos = None;
        self.pending_title_task = None;
        self.view = AiPanelView::Chat;
        self.focus_handle.focus(window, cx);
        self.refresh_sessions(cx);
        cx.notify();
    }

    fn load_session(&mut self, session_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let Some(session) = self
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .cloned()
        else {
            return;
        };

        self.update_current_page(cx);
        self.save_current_session(cx);
        self.session_id = session.id;
        self.session_title = session.title;
        self.session_created = session.created;
        self.session_updated = session.updated;
        self.current_page = session.page;
        self.messages = session
            .messages
            .into_iter()
            .map(|message| ChatMessage {
                role: match message.role {
                    AiSessionRole::User => Role::User,
                    AiSessionRole::Assistant => Role::Assistant,
                },
                content: message.content,
                request_content: message.request_content,
            })
            .collect();
        self.content = SharedString::default();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.mention_completion = None;
        self.last_cursor_pos = None;
        self.pending_title_task = None;
        self.view = AiPanelView::Chat;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn update_mention_completion(&mut self) {
        let Some(source_range) = self.active_mention_source_range() else {
            self.mention_completion = None;
            return;
        };
        let matches = self.mention_matches_for_range(&source_range);
        if matches.is_empty() {
            self.mention_completion = None;
            return;
        }

        let selected_ix = self
            .mention_completion
            .as_ref()
            .map(|completion| completion.selected_ix)
            .unwrap_or(0)
            .min(matches.len().saturating_sub(1));
        self.mention_completion = Some(MentionCompletion {
            source_range,
            selected_ix,
        });
    }

    fn active_mention_source_range(&self) -> Option<Range<usize>> {
        if !self.selected_range.is_empty() {
            return None;
        }

        let cursor = self.cursor_offset().min(self.content.len());
        let line_start = self.content[..cursor]
            .rfind('\n')
            .map(|ix| ix + '\n'.len_utf8())
            .unwrap_or(0);
        let line = &self.content[line_start..cursor];
        let at_offset = line.rfind('@')?;
        Some(line_start + at_offset..cursor)
    }

    fn mention_matches_for_range(&self, source_range: &Range<usize>) -> Vec<MentionKind> {
        if source_range.start >= source_range.end || source_range.end > self.content.len() {
            return Vec::new();
        }
        let query_start = source_range.start + '@'.len_utf8();
        if query_start > source_range.end {
            return Vec::new();
        }
        let query = &self.content[query_start..source_range.end];
        MentionKind::ALL
            .into_iter()
            .filter(|mention| mention_matches_query(*mention, query))
            .collect()
    }

    fn mention_completion_matches(&self) -> Vec<MentionKind> {
        self.mention_completion
            .as_ref()
            .map(|completion| self.mention_matches_for_range(&completion.source_range))
            .unwrap_or_default()
    }

    fn confirm_mention_completion(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(completion) = self.mention_completion.clone() else {
            return false;
        };
        let matches = self.mention_matches_for_range(&completion.source_range);
        let Some(mention) = matches.get(completion.selected_ix).copied() else {
            return false;
        };
        self.insert_mention_at_range(mention, completion.source_range, cx);
        true
    }

    fn select_previous_mention_completion(&mut self, cx: &mut Context<Self>) -> bool {
        let match_count = self.mention_completion_matches().len();
        let Some(completion) = self.mention_completion.as_mut() else {
            return false;
        };
        if match_count == 0 {
            return false;
        }
        completion.selected_ix = if completion.selected_ix == 0 {
            match_count - 1
        } else {
            completion.selected_ix - 1
        };
        cx.notify();
        true
    }

    fn select_next_mention_completion(&mut self, cx: &mut Context<Self>) -> bool {
        let match_count = self.mention_completion_matches().len();
        let Some(completion) = self.mention_completion.as_mut() else {
            return false;
        };
        if match_count == 0 {
            return false;
        }
        completion.selected_ix = (completion.selected_ix + 1) % match_count;
        cx.notify();
        true
    }

    fn insert_mention(&mut self, mention: MentionKind, cx: &mut Context<Self>) {
        let range = self
            .mention_completion
            .as_ref()
            .map(|completion| completion.source_range.clone())
            .unwrap_or_else(|| {
                let cursor = self.cursor_offset().min(self.content.len());
                cursor..cursor
            });
        self.insert_mention_at_range(mention, range, cx);
    }

    fn open_context_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        if self.mention_completion.is_some() {
            return;
        }

        let cursor = self.cursor_offset().min(self.content.len());
        let needs_space = cursor > 0
            && self.content[..cursor]
                .chars()
                .last()
                .is_some_and(|ch| !ch.is_whitespace());
        let trigger = if needs_space { " @" } else { "@" };
        self.replace_text_in_range(None, trigger, window, cx);
    }

    fn insert_mention_at_range(
        &mut self,
        mention: MentionKind,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) {
        let new_text = format!("{} ", mention.token());
        self.content =
            (self.content[0..range.start].to_owned() + &new_text + &self.content[range.end..])
                .into();
        let cursor = range.start + new_text.len();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        self.mention_completion = None;
        cx.notify();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        self.is_selecting = true;

        let offset = self.index_for_mouse_position(event.position, window);
        if event.modifiers.shift {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
        cx.stop_propagation();
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = false;
        cx.stop_propagation();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position, window), cx);
            cx.stop_propagation();
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = offset.min(self.content.len());
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.update_mention_completion();
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = offset.min(self.content.len());
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        };

        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }

        self.update_mention_completion();
        cx.notify();
    }

    fn move_vertically(
        &mut self,
        direction: isize,
        extend_selection: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(bounds), Some(cursor)) = (
            self.last_bounds,
            self.position_for_offset(self.cursor_offset(), window),
        ) else {
            return;
        };

        let line_height = window.line_height();
        let target = point(
            bounds.left() + cursor.x,
            bounds.top() + cursor.y + line_height * direction as f32,
        );
        let offset = self.index_for_mouse_position(target, window);
        if extend_selection {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
    }

    fn line_start_offset(&self) -> usize {
        let cursor = self.cursor_offset();
        self.content[..cursor]
            .rfind('\n')
            .map(|ix| ix + '\n'.len_utf8())
            .unwrap_or(0)
    }

    fn line_end_offset(&self) -> usize {
        let cursor = self.cursor_offset();
        self.content[cursor..]
            .find('\n')
            .map(|ix| cursor + ix)
            .unwrap_or(self.content.len())
    }

    fn copy_selected_text(&self, cx: &mut Context<Self>) -> bool {
        if self.selected_range.is_empty() {
            return false;
        }

        cx.write_to_clipboard(ClipboardItem::new_string(
            self.content[self.selected_range.clone()].to_string(),
        ));
        true
    }

    fn index_for_mouse_position(&self, position: GpuiPoint<Pixels>, window: &mut Window) -> usize {
        if self.content.is_empty() {
            return 0;
        }

        let Some(bounds) = self.last_bounds else {
            return 0;
        };

        let line_height = window.line_height();
        let local = match bounds.localize(&position) {
            Some(local) => local,
            None if position.y < bounds.top() => return 0,
            None => return self.content.len(),
        };

        let mut line_top = px(0.0);
        for laid_out in &self.last_lines {
            let line_height_px = laid_out.line.size(line_height).height;
            if local.y <= line_top + line_height_px {
                let line_point = point(local.x, local.y - line_top);
                let local_ix = laid_out
                    .line
                    .closest_index_for_position(line_point, line_height)
                    .unwrap_or_else(|ix| ix);
                return (laid_out.start + local_ix).min(self.content.len());
            }
            line_top += line_height_px;
        }

        self.content.len()
    }

    fn position_for_offset(&self, offset: usize, window: &mut Window) -> Option<GpuiPoint<Pixels>> {
        let line_height = window.line_height();
        let mut line_top = px(0.0);
        for laid_out in &self.last_lines {
            let line_end = laid_out.start + laid_out.line.len();
            if offset <= line_end {
                let local_offset = offset.saturating_sub(laid_out.start);
                return laid_out
                    .line
                    .position_for_index(local_offset, line_height)
                    .map(|position| point(position.x, line_top + position.y));
            }
            line_top += laid_out.line.size(line_height).height;
        }

        self.last_lines.last().and_then(|line| {
            line.line
                .position_for_index(line.line.len(), line_height)
                .map(|position| point(position.x, line_top + position.y))
        })
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        offset_from_utf16_in_text(&self.content, offset)
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        offset_to_utf16_in_text(&self.content, offset)
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .char_indices()
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .char_indices()
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }

    fn render_message(message: ChatMessage, theme: crate::theme::Theme) -> impl IntoElement {
        let is_user = message.role == Role::User;
        let label = if is_user { "You" } else { "Assistant" };
        let content = message.content;
        let is_loading = content.is_empty();

        div()
            .flex()
            .flex_col()
            .gap_1()
            .items_start()
            .when(is_user, |this| this.items_end())
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(theme.text_muted))
                    .child(label),
            )
            .child(
                div()
                    .max_w(px(280.0))
                    .when(is_user, |this| {
                        this.rounded_md()
                            .border_1()
                            .border_color(rgba(with_alpha(theme.accent, 0x55)))
                            .bg(rgba(with_alpha(theme.accent, 0x22)))
                            .px_2()
                            .py_1()
                    })
                    .when(!is_user, |this| this.px_1().py_1())
                    .text_size(px(13.0))
                    .text_color(rgb(theme.text))
                    .when(is_loading, |this| {
                        this.child(Self::render_loading_indicator(theme))
                    })
                    .when(!is_loading, |this| this.child(render_text_lines(&content))),
            )
    }

    fn render_loading_indicator(theme: crate::theme::Theme) -> impl IntoElement {
        let dot = move |opacity| {
            div()
                .w(px(4.0))
                .h(px(4.0))
                .rounded_sm()
                .bg(rgb(theme.text_muted))
                .opacity(opacity)
        };

        div()
            .flex()
            .items_center()
            .gap_1()
            .h(px(18.0))
            .child(dot(0.45))
            .child(dot(0.7))
            .child(dot(1.0))
    }

    fn render_sessions_view(
        sessions: Vec<AiSession>,
        current_session_id: u64,
        loading: bool,
        theme: crate::theme::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut list = div().flex().flex_col().gap_1().p_2();
        list = list.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .rounded_sm()
                .border_1()
                .border_color(rgb(theme.accent))
                .bg(rgba(with_alpha(theme.accent, 0x18)))
                .px_2()
                .py_2()
                .text_size(px(13.0))
                .text_color(if loading {
                    rgb(theme.text_muted)
                } else {
                    rgb(theme.text)
                })
                .cursor_pointer()
                .hover(move |style| style.bg(rgba(with_alpha(theme.accent, 0x24))))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.new_session(window, cx);
                        cx.stop_propagation();
                    }),
                )
                .child("New chat")
                .child("+"),
        );

        if sessions.is_empty() {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(120.0))
                    .text_size(px(13.0))
                    .text_color(rgb(theme.text_muted))
                    .child("No previous AI sessions for this PDF"),
            );
        }

        for session in sessions {
            let active = session.id == current_session_id;
            let session_id = session.id;
            let title = session.title.clone();
            let meta = format!("p. {} · {} messages", session.page, session.messages.len());
            list = list.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .rounded_sm()
                    .px_2()
                    .py_2()
                    .bg(if active {
                        rgba(theme.selected_row)
                    } else {
                        rgba(0x00000000)
                    })
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(theme.control_hover)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.load_session(session_id, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.0))
                            .text_color(rgb(if active { theme.accent } else { theme.text }))
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(meta),
                    ),
            );
        }

        div()
            .id("ai-panel-sessions")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(list)
    }

    fn render_mention_completion_menu(
        matches: Vec<MentionKind>,
        selected_ix: usize,
        left: Pixels,
        top: Pixels,
        theme: crate::theme::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut menu = div()
            .absolute()
            .top(top)
            .left(left)
            .w(px(COMPLETION_MENU_WIDTH))
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(rgb(theme.panel_border))
            .bg(rgb(theme.control_background))
            .shadow_lg()
            .overflow_hidden()
            .text_size(px(12.0));

        for (ix, mention) in matches.into_iter().enumerate() {
            let selected = ix == selected_ix;
            menu = menu.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .min_h(px(COMPLETION_MENU_ROW_HEIGHT))
                    .px_3()
                    .py_2()
                    .bg(if selected {
                        rgba(theme.selected_row)
                    } else {
                        rgba(0x00000000)
                    })
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(theme.control_hover)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.insert_mention(mention, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_color(rgb(if selected {
                                        theme.accent
                                    } else {
                                        theme.text
                                    }))
                                    .child("@"),
                            )
                            .child(
                                div()
                                    .text_color(rgb(if selected {
                                        theme.accent
                                    } else {
                                        theme.text
                                    }))
                                    .child(mention.label()),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(mention.description()),
                    ),
            );
        }

        menu
    }
}

impl EntityInputHandler for AiPanel {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range.take();
        self.update_mention_completion();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let selected_range_start = range.start;

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.marked_range =
            (!new_text.is_empty()).then_some(range.start..range.start + new_text.len());
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| {
                let start = offset_from_utf16_in_text(new_text, range_utf16.start);
                let end = offset_from_utf16_in_text(new_text, range_utf16.end);
                selected_range_start + start..selected_range_start + end
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;
        self.update_mention_completion();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        let start = self.position_for_offset(range.start, window)?;
        let end = self.position_for_offset(range.end, window)?;
        Some(Bounds::from_corners(
            point(bounds.left() + start.x, bounds.top() + start.y),
            point(
                bounds.left() + end.x,
                bounds.top() + end.y + window.line_height(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: GpuiPoint<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.offset_to_utf16(self.index_for_mouse_position(point, window)))
    }
}

impl EventEmitter<PanelEvent> for AiPanel {}

impl Focusable for AiPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AiPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let theme = cx.global::<SettingsStore>().resolved_theme;
        let can_send = !self.loading && !self.content.trim().is_empty();
        let completion_matches = self.mention_completion_matches();
        let completion_selected = self
            .mention_completion
            .as_ref()
            .map(|completion| completion.selected_ix)
            .unwrap_or(0);
        let sessions = self.sessions.clone();
        let current_session_id = self.session_id;
        let session_title = self.session_title.clone();
        let showing_sessions = self.view == AiPanelView::Sessions;
        let loading = self.loading;
        let mention_completion_open = self.mention_completion.is_some();
        let completion_menu_height = (completion_matches.len() as f32 * COMPLETION_MENU_ROW_HEIGHT)
            .max(COMPLETION_MENU_ROW_HEIGHT);
        let completion_left = self
            .last_cursor_pos
            .map(|position| {
                let max_left = px((AI_PANEL_WIDTH - COMPLETION_MENU_WIDTH - 24.0).max(0.0));
                if position.x < px(0.0) {
                    px(0.0)
                } else if position.x > max_left {
                    max_left
                } else {
                    position.x
                }
            })
            .unwrap_or(px(0.0));
        let completion_top = self
            .last_cursor_pos
            .map(|position| position.y - px(completion_menu_height + 8.0))
            .unwrap_or(px(-completion_menu_height - 8.0));

        let mut messages = div().flex().flex_col().gap_3().p_3();
        if self.messages.is_empty() {
            messages = messages.child(
                div()
                    .text_size(px(13.0))
                    .text_color(rgb(theme.text_muted))
                    .child("Ask questions about the PDF. Type @ to add selection or current page text."),
            );
        }
        for message in self.messages.clone() {
            messages = messages.child(Self::render_message(message, theme));
        }

        div()
            .key_context("AiPanel")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .flex_none()
            .w(px(AI_PANEL_WIDTH))
            .h_full()
            .border_l(px(1.0))
            .border_color(rgb(theme.panel_border))
            .bg(rgb(theme.panel_background))
            .text_color(rgb(theme.text))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(42.0))
                    .px_2()
                    .border_b(px(1.0))
                    .border_color(rgb(theme.panel_border))
                    .text_size(px(12.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(rgb(theme.text_muted))
                                    .child("AI"),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(13.0))
                                    .text_color(rgb(theme.text))
                                    .child(session_title),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .w(px(24.0))
                                    .h(px(24.0))
                                    .rounded_sm()
                                    .text_size(px(14.0))
                                    .text_color(if loading {
                                        rgb(theme.text_muted)
                                    } else {
                                        rgb(theme.text)
                                    })
                                    .cursor_pointer()
                                    .hover(move |style| style.bg(rgba(theme.control_hover)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, window, cx| {
                                            this.new_session(window, cx);
                                            cx.stop_propagation();
                                        }),
                                    )
                                    .child("+"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .w(px(24.0))
                                    .h(px(24.0))
                                    .rounded_sm()
                                    .text_size(px(14.0))
                                    .text_color(rgb(if showing_sessions {
                                        theme.accent
                                    } else {
                                        theme.text_muted
                                    }))
                                    .cursor_pointer()
                                    .hover(move |style| style.bg(rgba(theme.control_hover)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            if this.view == AiPanelView::Sessions {
                                                this.show_chat(cx);
                                            } else {
                                                this.show_sessions(cx);
                                            }
                                            cx.stop_propagation();
                                        }),
                                    )
                                    .child(if showing_sessions { "←" } else { "≡" }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .w(px(24.0))
                                    .h(px(24.0))
                                    .rounded_sm()
                                    .text_size(px(14.0))
                                    .text_color(rgb(theme.text_muted))
                                    .cursor_pointer()
                                    .hover(move |style| style.bg(rgba(theme.control_hover)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, window, cx| {
                                            this.dismiss(window, cx);
                                            cx.stop_propagation();
                                        }),
                                    )
                                    .child("×"),
                            ),
                    ),
            )
            .when(showing_sessions, |this| {
                this.child(Self::render_sessions_view(
                    sessions,
                    current_session_id,
                    loading,
                    theme,
                    cx,
                ))
            })
            .when(!showing_sessions, |this| {
                this.child(
                    div()
                        .id("ai-panel-messages")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll_handle)
                        .child(messages),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .p_2()
                        .border_t(px(1.0))
                        .border_color(rgb(theme.panel_border))
                        .child(
                            div()
                                .relative()
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(theme.control_border))
                                .bg(rgb(theme.control_background))
                                .p_2()
                                .cursor(CursorStyle::IBeam)
                                .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                                .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                                .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
                                .on_mouse_move(cx.listener(Self::on_mouse_move))
                                .child(AiInputElement { panel: entity })
                                .when(!completion_matches.is_empty(), |this| {
                                    this.child(Self::render_mention_completion_menu(
                                        completion_matches,
                                        completion_selected,
                                        completion_left,
                                        completion_top,
                                        theme,
                                        cx,
                                    ))
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .text_size(px(11.0))
                                .text_color(rgb(theme.text_muted))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .child(
                                            div()
                                                .px_2()
                                                .py_1()
                                                .rounded_sm()
                                                .text_size(px(11.0))
                                                .text_color(rgb(if mention_completion_open {
                                                    theme.accent
                                                } else {
                                                    theme.text_muted
                                                }))
                                                .cursor_pointer()
                                                .hover(move |style| {
                                                    style.bg(rgba(theme.control_hover))
                                                })
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(|this, _, window, cx| {
                                                        this.open_context_completion(window, cx);
                                                        cx.stop_propagation();
                                                    }),
                                                )
                                                .child("@ Add context"),
                                        )
                                        .child(if mention_completion_open {
                                            "Enter inserts context"
                                        } else {
                                            "Ctrl+Enter sends"
                                        }),
                                )
                                .child(
                                    div()
                                        .px_3()
                                        .py_1()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(if can_send {
                                            rgb(theme.accent)
                                        } else {
                                            rgb(theme.control_border)
                                        })
                                        .bg(if can_send {
                                            rgba(with_alpha(theme.accent, 0x22))
                                        } else {
                                            rgba(theme.control_background)
                                        })
                                        .text_color(if can_send {
                                            rgb(theme.text)
                                        } else {
                                            rgb(theme.text_muted)
                                        })
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                this.send_message(cx);
                                                cx.stop_propagation();
                                            }),
                                        )
                                        .child(if loading { "Sending…" } else { "Send" }),
                                ),
                        ),
                )
            })
    }
}

impl IntoElement for AiInputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for AiInputElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let content = self.panel.read(cx).content.clone();
        let text = content.as_ref();
        let estimated_lines = if text.is_empty() {
            3
        } else {
            text.lines()
                .map(|line| line.chars().count().saturating_sub(1) / 36 + 1)
                .sum::<usize>()
                + usize::from(text.ends_with('\n'))
        }
        .clamp(3, 8);

        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = (window.line_height() * estimated_lines as f32).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.panel.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();
        let theme = cx.global::<SettingsStore>().resolved_theme;
        let (display_text, text_color) = if content.is_empty() {
            (
                "Ask about this PDF…".into(),
                rgba(theme.placeholder_text).into(),
            )
        } else {
            (content, style.color)
        };

        let base_run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if !input.content.is_empty() {
            input_text_runs(
                display_text.as_ref(),
                base_run,
                theme,
                input.marked_range.as_ref(),
            )
        } else {
            vec![base_run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let lines = match window.text_system().shape_text(
            display_text.clone(),
            font_size,
            &runs,
            Some(bounds.size.width),
            None,
        ) {
            Ok(lines) => lines,
            Err(error) => {
                eprintln!("failed to shape AI input: {error:#}");
                return PrepaintState {
                    lines: Some(Vec::new()),
                    cursor: None,
                    cursor_position: None,
                    selections: Vec::new(),
                };
            }
        };

        let mut laid_out_lines = Vec::with_capacity(lines.len());
        let mut starts = line_starts(display_text.as_ref());
        for line in lines {
            let start = starts.next().unwrap_or(0);
            laid_out_lines.push(LaidOutLine { start, line });
        }

        let cursor_position = if selected_range.is_empty() {
            cursor_position_for_lines(&laid_out_lines, cursor, line_height)
        } else {
            None
        };

        let selections = if input.content.is_empty() || selected_range.is_empty() {
            Vec::new()
        } else {
            selection_quads(
                &laid_out_lines,
                &selected_range,
                bounds,
                line_height,
                rgba(theme.selection_highlight).into(),
            )
        };

        let cursor = if input.focus_handle.is_focused(window) && selected_range.is_empty() {
            cursor_quad(
                &laid_out_lines,
                cursor,
                bounds,
                line_height,
                rgb(theme.accent).into(),
            )
        } else {
            None
        };

        PrepaintState {
            lines: Some(laid_out_lines),
            cursor,
            cursor_position,
            selections,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.panel.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.panel.clone()),
            cx,
        );

        for selection in prepaint.selections.drain(..) {
            window.paint_quad(selection);
        }

        let Some(lines) = prepaint.lines.take() else {
            return;
        };

        let line_height = window.line_height();
        let mut y = bounds.top();
        for laid_out in &lines {
            let line_bounds = Bounds::new(
                point(bounds.left(), y),
                size(bounds.size.width, line_height),
            );
            if let Err(error) = laid_out.line.paint(
                point(bounds.left(), y),
                line_height,
                gpui::TextAlign::Left,
                Some(line_bounds),
                window,
                cx,
            ) {
                eprintln!("failed to paint AI input: {error:#}");
            }
            y += laid_out.line.size(line_height).height;
        }

        if let Some(cursor) = prepaint.cursor.take() {
            window.paint_quad(cursor);
        }

        self.panel.update(cx, |input, _| {
            input.last_lines = lines;
            input.last_bounds = Some(bounds);
            input.last_cursor_pos = prepaint.cursor_position;
        });
    }
}

fn stream_completion(
    settings: AiSettings,
    messages: Vec<ChatMessage>,
    send_event: &mut impl FnMut(AiStreamEvent) -> Result<()>,
) -> Result<()> {
    if settings.provider == AiProvider::Anthropic && settings.api_key.is_empty() {
        return Err(anyhow!("missing Anthropic API key in ai.api_key"));
    }
    if settings.provider == AiProvider::OpenAi
        && settings.api_key.is_empty()
        && settings.base_url.is_none()
    {
        return Err(anyhow!(
            "missing OpenAI API key in ai.api_key, or set ai.base_url for a local/OpenAI-compatible server"
        ));
    }

    let client = reqwest::blocking::Client::builder()
        .build()
        .context("failed to create HTTP client")?;

    match settings.provider {
        AiProvider::OpenAi => stream_openai(&client, settings, messages, send_event),
        AiProvider::Anthropic => stream_anthropic(&client, settings, messages, send_event),
    }
}

fn stream_openai(
    client: &reqwest::blocking::Client,
    settings: AiSettings,
    messages: Vec<ChatMessage>,
    send_event: &mut impl FnMut(AiStreamEvent) -> Result<()>,
) -> Result<()> {
    let url = openai_chat_url(settings.base_url.as_deref());
    let mut request_messages = Vec::new();
    request_messages.push(json!({
        "role": "system",
        "content": system_prompt(),
    }));
    for message in messages {
        request_messages.push(json!({
            "role": match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            },
            "content": message.request_content.unwrap_or(message.content),
        }));
    }

    let mut request = client.post(url).json(&json!({
        "model": settings.model,
        "messages": request_messages,
        "stream": true,
    }));
    if !settings.api_key.is_empty() {
        request = request.bearer_auth(settings.api_key);
    }

    let response = request.send().context("failed to send OpenAI request")?;
    read_sse_response(response, AiProvider::OpenAi, send_event)
}

fn stream_anthropic(
    client: &reqwest::blocking::Client,
    settings: AiSettings,
    messages: Vec<ChatMessage>,
    send_event: &mut impl FnMut(AiStreamEvent) -> Result<()>,
) -> Result<()> {
    let base_url = settings
        .base_url
        .as_deref()
        .unwrap_or("https://api.anthropic.com")
        .trim_end_matches('/');
    let url = if base_url.ends_with("/v1/messages") {
        base_url.to_string()
    } else if base_url.ends_with("/v1") {
        format!("{base_url}/messages")
    } else {
        format!("{base_url}/v1/messages")
    };

    let request_messages = messages
        .into_iter()
        .map(|message| {
            json!({
                "role": match message.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                },
                "content": message.request_content.unwrap_or(message.content),
            })
        })
        .collect::<Vec<_>>();

    let mut body = json!({
        "model": settings.model,
        "messages": request_messages,
        "max_tokens": 4096,
        "stream": true,
    });
    body["system"] = json!(system_prompt());

    let response = client
        .post(url)
        .header("x-api-key", settings.api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .context("failed to send Anthropic request")?;
    read_sse_response(response, AiProvider::Anthropic, send_event)
}

fn read_sse_response(
    mut response: reqwest::blocking::Response,
    provider: AiProvider,
    send_event: &mut impl FnMut(AiStreamEvent) -> Result<()>,
) -> Result<()> {
    if !response.status().is_success() {
        let status = response.status();
        let mut body = String::new();
        let _ = response.read_to_string(&mut body);
        return Err(anyhow!("AI request failed with status {status}: {body}"));
    }

    let mut pending = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = response
            .read(&mut buffer)
            .context("failed to read AI stream")?;
        if read == 0 {
            break;
        }
        pending.extend_from_slice(&buffer[..read]);

        while let Some(newline_ix) = pending.iter().position(|byte| *byte == b'\n') {
            let line = pending.drain(..=newline_ix).collect::<Vec<_>>();
            let line = std::str::from_utf8(&line)
                .context("AI stream emitted invalid UTF-8")?
                .trim();
            if process_sse_line(provider, line, send_event)? {
                return Ok(());
            }
        }
    }

    if !pending.is_empty() {
        let line = std::str::from_utf8(&pending)
            .context("AI stream emitted invalid UTF-8")?
            .trim();
        process_sse_line(provider, line, send_event)?;
    }

    Ok(())
}

fn process_sse_line(
    provider: AiProvider,
    line: &str,
    send_event: &mut impl FnMut(AiStreamEvent) -> Result<()>,
) -> Result<bool> {
    let Some(data) = line.strip_prefix("data:") else {
        return Ok(false);
    };
    let data = data.trim();
    if data == "[DONE]" {
        return Ok(true);
    }

    let value = serde_json::from_str::<Value>(data).context("failed to parse AI stream event")?;
    match provider {
        AiProvider::OpenAi => {
            if let Some(delta) = value["choices"][0]["delta"]["content"].as_str()
                && !delta.is_empty()
            {
                send_event(AiStreamEvent::Delta(delta.to_string()))?;
            }
            Ok(false)
        }
        AiProvider::Anthropic => {
            match value["type"].as_str() {
                Some("content_block_delta") => {
                    if let Some(delta) = value["delta"]["text"].as_str()
                        && !delta.is_empty()
                    {
                        send_event(AiStreamEvent::Delta(delta.to_string()))?;
                    }
                }
                Some("message_stop") => return Ok(true),
                _ => {}
            }
            Ok(false)
        }
    }
}

fn openai_chat_url(base_url: Option<&str>) -> String {
    let base_url = base_url
        .unwrap_or("https://api.openai.com")
        .trim_end_matches('/');
    if base_url.ends_with("/v1/chat/completions") {
        base_url.to_string()
    } else if base_url.ends_with("/v1") {
        format!("{base_url}/chat/completions")
    } else {
        format!("{base_url}/v1/chat/completions")
    }
}

fn system_prompt() -> &'static str {
    "You are an AI assistant embedded in a PDF viewer. Help the user understand and work with the document. Be conversational but professional. Refer to the user in the second person and yourself in the first person. Format responses in Markdown. Never invent document details; if the needed PDF context is missing, say what context you need. When the user includes @selection or @current page text context, ground your answer in that attached context."
}

fn mentioned_kinds(text: &str) -> Vec<MentionKind> {
    let mut seen = HashSet::new();
    let mut mentions = Vec::new();
    for (_, mention) in find_mention_ranges(text) {
        if seen.insert(mention) {
            mentions.push(mention);
        }
    }
    mentions
}

fn find_mention_ranges(text: &str) -> Vec<(Range<usize>, MentionKind)> {
    let mut ranges = Vec::new();
    for (start, ch) in text.char_indices() {
        if ch != '@' {
            continue;
        }
        for mention in MentionKind::ALL {
            let token = mention.token();
            if text[start..].starts_with(token) {
                let end = start + token.len();
                if is_mention_boundary(text[end..].chars().next()) {
                    ranges.push((start..end, mention));
                    break;
                }
            }
        }
    }
    ranges
}

fn is_mention_boundary(next: Option<char>) -> bool {
    next.is_none_or(|ch| {
        ch.is_whitespace() || matches!(ch, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}')
    })
}

fn mention_matches_query(mention: MentionKind, query: &str) -> bool {
    let query = query.trim_start().to_ascii_lowercase();
    if query.is_empty() {
        return true;
    }

    let query_without_trailing = query.trim_end();
    let label = mention.label().to_ascii_lowercase();
    let token = mention.token().trim_start_matches('@').to_ascii_lowercase();

    if query.ends_with(' ') && (query_without_trailing == label || query_without_trailing == token)
    {
        return false;
    }

    label.contains(query_without_trailing) || token.contains(query_without_trailing)
}

fn truncate_context(text: String) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.chars().take(MAX_CONTEXT_CHARS).collect())
}

fn session_title_for_prompt(prompt: &str) -> String {
    let title = prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(DEFAULT_SESSION_TITLE)
        .trim()
        .chars()
        .take(48)
        .collect::<String>();
    if title.is_empty() {
        DEFAULT_SESSION_TITLE.to_string()
    } else {
        title
    }
}

fn title_from_response(response: &str) -> Option<String> {
    let title = response
        .lines()
        .find(|line| !line.trim().is_empty())?
        .trim()
        .trim_matches(['"', '\'', '`'])
        .trim()
        .trim_end_matches(['.', '!', '?', ':'])
        .chars()
        .take(80)
        .collect::<String>();
    (!title.is_empty()).then_some(title)
}

fn input_text_runs(
    text: &str,
    base_run: TextRun,
    theme: crate::theme::Theme,
    marked_range: Option<&Range<usize>>,
) -> Vec<TextRun> {
    let mention_ranges = find_mention_ranges(text);
    let mut boundaries = vec![0, text.len()];
    for (range, _) in &mention_ranges {
        boundaries.push(range.start);
        boundaries.push(range.end);
    }
    if let Some(range) = marked_range {
        boundaries.push(range.start);
        boundaries.push(range.end);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    boundaries
        .windows(2)
        .filter_map(|window| {
            let start = window[0];
            let end = window[1];
            if start >= end {
                return None;
            }

            let mention = mention_ranges
                .iter()
                .any(|(range, _)| start >= range.start && end <= range.end);
            let marked = marked_range
                .as_ref()
                .is_some_and(|range| start >= range.start && end <= range.end);
            let mut run = TextRun {
                len: end - start,
                ..base_run.clone()
            };
            if mention {
                run.color = rgb(theme.accent).into();
            }
            if marked {
                run.underline = Some(gpui::UnderlineStyle {
                    color: Some(run.color),
                    thickness: px(1.0),
                    wavy: false,
                });
            }
            Some(run)
        })
        .collect()
}

fn render_text_lines(content: &str) -> impl IntoElement {
    let mut lines = div().flex().flex_col().gap_1();
    for line in content.lines() {
        lines = lines.child(if line.is_empty() {
            " ".to_string()
        } else {
            line.to_string()
        });
    }
    if content.ends_with('\n') {
        lines = lines.child(" ".to_string());
    }
    lines
}

fn line_starts(text: &str) -> impl Iterator<Item = usize> + '_ {
    std::iter::once(0).chain(
        text.char_indices()
            .filter_map(|(ix, ch)| (ch == '\n').then_some(ix + ch.len_utf8())),
    )
}

fn cursor_position_for_lines(
    lines: &[LaidOutLine],
    cursor: usize,
    line_height: Pixels,
) -> Option<GpuiPoint<Pixels>> {
    let mut line_top = px(0.0);
    for laid_out in lines {
        let line_end = laid_out.start + laid_out.line.len();
        if cursor <= line_end {
            let local_cursor = cursor.saturating_sub(laid_out.start);
            return laid_out
                .line
                .position_for_index(local_cursor, line_height)
                .map(|position| point(position.x, line_top + position.y));
        }
        line_top += laid_out.line.size(line_height).height;
    }

    lines.last().and_then(|line| {
        line.line
            .position_for_index(line.line.len(), line_height)
            .map(|position| point(position.x, line_top + position.y))
    })
}

fn cursor_quad(
    lines: &[LaidOutLine],
    cursor: usize,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    color: gpui::Hsla,
) -> Option<PaintQuad> {
    let mut line_top = px(0.0);
    for laid_out in lines {
        let line_end = laid_out.start + laid_out.line.len();
        if cursor <= line_end {
            let local_cursor = cursor.saturating_sub(laid_out.start);
            let position = laid_out
                .line
                .position_for_index(local_cursor, line_height)?;
            return Some(fill(
                Bounds::new(
                    point(
                        bounds.left() + position.x,
                        bounds.top() + line_top + position.y,
                    ),
                    size(px(2.0), line_height),
                ),
                color,
            ));
        }
        line_top += laid_out.line.size(line_height).height;
    }

    None
}

fn selection_quads(
    lines: &[LaidOutLine],
    selected_range: &Range<usize>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    color: gpui::Hsla,
) -> Vec<PaintQuad> {
    let mut quads = Vec::new();
    let mut line_top = px(0.0);
    for laid_out in lines {
        let line_start = laid_out.start;
        let line_end = laid_out.start + laid_out.line.len();
        let start = selected_range.start.max(line_start);
        let end = selected_range.end.min(line_end);
        if start < end || (start == end && selected_range.contains(&start)) {
            let local_start = start.saturating_sub(line_start);
            let local_end = end.saturating_sub(line_start);
            let Some(start_pos) = laid_out.line.position_for_index(local_start, line_height) else {
                line_top += laid_out.line.size(line_height).height;
                continue;
            };
            let Some(end_pos) = laid_out.line.position_for_index(local_end, line_height) else {
                line_top += laid_out.line.size(line_height).height;
                continue;
            };

            let start_row = (start_pos.y / line_height) as usize;
            let end_row = (end_pos.y / line_height) as usize;
            for row in start_row..=end_row {
                let row_y = bounds.top() + line_top + line_height * row as f32;
                let left = if row == start_row {
                    bounds.left() + start_pos.x
                } else {
                    bounds.left()
                };
                let right = if row == end_row {
                    bounds.left() + end_pos.x
                } else {
                    bounds.left() + laid_out.line.width()
                };
                if right > left {
                    quads.push(fill(
                        Bounds::new(point(left, row_y), size(right - left, line_height)),
                        color,
                    ));
                }
            }
        }
        line_top += laid_out.line.size(line_height).height;
    }
    quads
}

fn offset_from_utf16_in_text(text: &str, offset: usize) -> usize {
    let mut utf8_offset = 0;
    let mut utf16_count = 0;

    for ch in text.chars() {
        if utf16_count >= offset {
            break;
        }
        utf16_count += ch.len_utf16();
        utf8_offset += ch.len_utf8();
    }

    utf8_offset
}

fn offset_to_utf16_in_text(text: &str, offset: usize) -> usize {
    let mut utf16_offset = 0;
    let mut utf8_count = 0;

    for ch in text.chars() {
        if utf8_count >= offset {
            break;
        }
        utf8_count += ch.len_utf8();
        utf16_offset += ch.len_utf16();
    }

    utf16_offset
}

fn with_alpha(rgb: u32, alpha: u8) -> u32 {
    (rgb << 8) | u32::from(alpha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pdf_context_mentions_in_order_without_duplicates() {
        assert_eq!(
            mentioned_kinds("Summarize @selection and compare with @current page text"),
            vec![MentionKind::Selection, MentionKind::CurrentPageText]
        );
        assert_eq!(
            mentioned_kinds("@selection then @selection again"),
            vec![MentionKind::Selection]
        );
    }

    #[test]
    fn mention_completion_keeps_current_page_space_query_open() {
        assert!(mention_matches_query(
            MentionKind::CurrentPageText,
            "current "
        ));
        assert!(mention_matches_query(MentionKind::CurrentPageText, "page"));
        assert!(!mention_matches_query(
            MentionKind::CurrentPageText,
            "current page text "
        ));
    }

    #[test]
    fn derives_session_title_from_first_prompt_line() {
        assert_eq!(
            session_title_for_prompt("\n  Explain the introduction in detail"),
            "Explain the introduction in detail"
        );
        assert_eq!(session_title_for_prompt(""), DEFAULT_SESSION_TITLE);
    }

    #[test]
    fn extracts_generated_title_from_first_response_line() {
        assert_eq!(
            title_from_response("  `Fourier Transform Basics`\n\nOther text"),
            Some("Fourier Transform Basics".to_string())
        );
        assert_eq!(title_from_response("   \n"), None);
    }
}
