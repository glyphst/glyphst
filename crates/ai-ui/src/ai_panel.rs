use std::{collections::HashSet, ops::Range, path::PathBuf, thread};

use crate::{AiContext, AiContextProvider, AiPageState, ResolvedPageContext};
use anyhow::anyhow;
use futures::{StreamExt, channel::mpsc};
use glyphst_actions::{
    ai_panel::{AiPanelClose, AiPanelSubmit},
    input,
};
use glyphst_ai::ai_chat::{
    AiStreamEvent, ChatMessage, ContextBlock, DEFAULT_SESSION_TITLE, DocumentContext, MentionKind,
    MentionReference, Role, TITLE_GENERATION_PROMPT, find_mention_references,
    mention_matches_query, session_title_for_prompt, stream_completion, thinking_level_description,
    title_from_response,
};
use glyphst_library::{
    AiMentionReference, AiSession, AiSessionMessage, AiSessionRole, LibraryStore, current_timestamp,
};
use glyphst_settings::{
    AiModel, AiProvider, AiThinkingLevel, ModelRef, SettingsStore, localize::tr, theme::Theme,
    ui_rems,
};
use glyphst_ui::{
    markdown::{Markdown, MarkdownElement, MarkdownStyle},
    scrollbar::WithScrollbar as _,
};
use gpui::{
    Anchor, App, BorderStyle, Bounds, ClipboardItem, Context, Corners, CursorStyle, Edges, Element,
    ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, FontWeight, GlobalElementId, InspectorElementId, IntoElement, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point as GpuiPoint, Render,
    ScrollHandle, SharedString, Style, Task, TextRun, TextStyle, UTF16Selection, Window,
    WrappedLine, anchored, deferred, div, fill, point, prelude::*, px, quad, relative, rgb, rgba,
    size,
};

pub const AI_PANEL_WIDTH: f32 = 360.0;
const COMPLETION_MENU_WIDTH: f32 = 248.0;
const COMPLETION_MENU_ROW_HEIGHT: f32 = 46.0;
const DRAG_SELECT_THRESHOLD: f32 = 2.0;
const MAX_PAGE_IMAGE_CONTEXTS: usize = 8;
const MAX_PAGE_IMAGE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiPanelEvent {
    Dismissed,
    Changed,
}

struct ModelMenuRenderState {
    selected_provider: AiProvider,
    selected_model: String,
    provider_models: Vec<(AiProvider, Vec<AiModel>)>,
    search_query: SharedString,
    scroll_handle: ScrollHandle,
    focused: bool,
}

#[derive(Clone)]
enum ModelMenuEntry {
    Header(AiProvider),
    Model(AiProvider, AiModel),
}

#[derive(Clone)]
struct MentionCompletion {
    source_range: Range<usize>,
    selected_ix: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ComposerMention {
    reference: MentionReference,
    range: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActivePageNumberEdit {
    mention_start: usize,
    digits: Range<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AiPanelView {
    Chat,
    Sessions,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AiPanelMode {
    Sidebar,
    QuickAsk,
}

pub struct AiPanel {
    focus_handle: FocusHandle,
    previous_focus_handle: FocusHandle,
    mode: AiPanelMode,
    messages: Vec<ChatMessage>,
    width: f32,
    content: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_lines: Vec<LaidOutLine>,
    last_bounds: Option<Bounds<Pixels>>,
    drag_origin: Option<GpuiPoint<Pixels>>,
    loading: bool,
    scroll_handle: ScrollHandle,
    sessions_scroll: ScrollHandle,
    pending_stream: Option<Task<()>>,
    pending_title_task: Option<Task<()>>,
    context_provider: Box<dyn AiContextProvider>,
    document_path: PathBuf,
    current_page: usize,
    page_count: usize,
    session_id: u64,
    session_title: String,
    session_created: u64,
    session_updated: u64,
    sessions: Vec<AiSession>,
    view: AiPanelView,
    mention_completion: Option<MentionCompletion>,
    composer_mentions: Vec<ComposerMention>,
    active_page_number_edit: Option<ActivePageNumberEdit>,
    last_cursor_pos: Option<GpuiPoint<Pixels>>,
    attachment_menu_open: bool,
    attachment_menu_anchor: Option<GpuiPoint<Pixels>>,
    model_menu_open: bool,
    model_menu_anchor: Option<GpuiPoint<Pixels>>,
    model_search_query: SharedString,
    model_menu_scroll: ScrollHandle,
    thinking_menu_open: bool,
    thinking_menu_anchor: Option<GpuiPoint<Pixels>>,
    markdown_views: Vec<Entity<Markdown>>,
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
    mention_quads: Vec<PaintQuad>,
    active_number_quads: Vec<PaintQuad>,
}

#[derive(Clone, Copy)]
struct DecorationStyle {
    background: gpui::Hsla,
    border: gpui::Hsla,
    border_width: Pixels,
    radius: Pixels,
}

struct AiPanelInit {
    previous_focus_handle: FocusHandle,
    context_provider: Box<dyn AiContextProvider>,
    document_path: PathBuf,
    page_state: AiPageState,
    width: f32,
    mode: AiPanelMode,
    session_id: u64,
    sessions: Vec<AiSession>,
}

impl AiPanel {
    pub fn new(
        previous_focus_handle: FocusHandle,
        context_provider: impl AiContextProvider,
        document_path: PathBuf,
        page_state: AiPageState,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Self {
        let session_id = cx.global_mut::<LibraryStore>().next_ai_session_id();
        let sessions = cx.global::<LibraryStore>().ai_sessions_for(&document_path);
        Self::new_with_mode(
            AiPanelInit {
                previous_focus_handle,
                context_provider: Box::new(context_provider),
                document_path,
                page_state,
                width,
                mode: AiPanelMode::Sidebar,
                session_id,
                sessions,
            },
            cx,
        )
    }

    pub fn new_quick_ask(
        previous_focus_handle: FocusHandle,
        context_provider: impl AiContextProvider,
        document_path: PathBuf,
        page_state: AiPageState,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_mode(
            AiPanelInit {
                previous_focus_handle,
                context_provider: Box::new(context_provider),
                document_path,
                page_state,
                width,
                mode: AiPanelMode::QuickAsk,
                session_id: 0,
                sessions: Vec::new(),
            },
            cx,
        )
    }

    fn new_with_mode(init: AiPanelInit, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            previous_focus_handle: init.previous_focus_handle,
            mode: init.mode,
            messages: Vec::new(),
            width: init.width,
            content: SharedString::default(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_lines: Vec::new(),
            last_bounds: None,
            drag_origin: None,
            loading: false,
            scroll_handle: ScrollHandle::new(),
            sessions_scroll: ScrollHandle::new(),
            pending_stream: None,
            pending_title_task: None,
            context_provider: init.context_provider,
            document_path: init.document_path,
            current_page: init.page_state.current_page,
            page_count: init.page_state.page_count,
            session_id: init.session_id,
            session_title: DEFAULT_SESSION_TITLE.to_string(),
            session_created: 0,
            session_updated: 0,
            sessions: init.sessions,
            view: AiPanelView::Chat,
            mention_completion: None,
            composer_mentions: Vec::new(),
            active_page_number_edit: None,
            last_cursor_pos: None,
            attachment_menu_open: false,
            attachment_menu_anchor: None,
            model_menu_open: false,
            model_menu_anchor: None,
            model_search_query: SharedString::default(),
            model_menu_scroll: ScrollHandle::new(),
            thinking_menu_open: false,
            thinking_menu_anchor: None,
            markdown_views: Vec::new(),
        }
    }

    pub fn set_width(&mut self, width: f32) {
        if (self.width - width).abs() > f32::EPSILON {
            self.last_lines.clear();
            self.last_bounds = None;
            self.last_cursor_pos = None;
        }
        self.width = width;
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_current_page(cx);
        if self.mode == AiPanelMode::Sidebar {
            self.save_current_session(cx);
        }
        self.previous_focus_handle.focus(window, cx);
        cx.emit(AiPanelEvent::Dismissed);
    }

    fn close(&mut self, _: &AiPanelClose, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(window, cx);
    }

    fn submit(&mut self, _: &AiPanelSubmit, _: &mut Window, cx: &mut Context<Self>) {
        if self.finish_page_number_edit(cx) {
            return;
        }
        if self.confirm_mention_completion(cx) {
            return;
        }
        self.send_message(cx);
    }

    fn newline(&mut self, _: &input::Newline, window: &mut Window, cx: &mut Context<Self>) {
        if self.finish_page_number_edit(cx) {
            return;
        }
        if self.mode == AiPanelMode::Sidebar && self.confirm_mention_completion(cx) {
            return;
        }
        self.replace_text_in_range(None, "\n", window, cx);
    }

    fn backspace(&mut self, _: &input::Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        if self.model_menu_open {
            let mut query = self.model_search_query.to_string();
            query.pop();
            self.model_search_query = query.into();
            cx.notify();
            return;
        }
        if let Some(active) = self.active_page_number_edit.clone() {
            let range = if self.selected_range.is_empty() {
                let cursor = self.cursor_offset();
                if cursor <= active.digits.start {
                    return;
                }
                self.previous_boundary(cursor)..cursor
            } else {
                self.selected_range.clone()
            };
            if self.apply_page_number_edit(range, "") {
                self.selection_reversed = false;
                self.marked_range = None;
                cx.notify();
            }
            return;
        }
        if self.selected_range.is_empty() {
            let cursor = self.cursor_offset();
            if let Some(mention) = self
                .composer_mentions
                .iter()
                .find(|mention| mention.range.end == cursor)
            {
                self.selected_range = mention.range.clone();
            } else {
                self.select_to(self.previous_boundary(cursor), cx);
            }
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &input::Delete, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        if self.model_menu_open {
            self.model_search_query = SharedString::default();
            cx.notify();
            return;
        }
        if let Some(active) = self.active_page_number_edit.clone() {
            let range = if self.selected_range.is_empty() {
                let cursor = self.cursor_offset();
                if cursor >= active.digits.end {
                    return;
                }
                cursor..self.next_boundary(cursor)
            } else {
                self.selected_range.clone()
            };
            if self.apply_page_number_edit(range, "") {
                self.selection_reversed = false;
                self.marked_range = None;
                cx.notify();
            }
            return;
        }
        if self.selected_range.is_empty() {
            let cursor = self.cursor_offset();
            if let Some(mention) = self
                .composer_mentions
                .iter()
                .find(|mention| mention.range.start == cursor)
            {
                self.selected_range = mention.range.clone();
            } else {
                self.select_to(self.next_boundary(cursor), cx);
            }
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn left(&mut self, _: &input::Left, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        if let Some(active) = self.active_page_number_edit.clone()
            && self.selected_range.is_empty()
            && self.cursor_offset() <= active.digits.start
        {
            self.finish_page_number_edit(cx);
            self.move_to(active.mention_start, cx);
            return;
        }
        if self.selected_range.is_empty() {
            let cursor = self.cursor_offset();
            let target = self
                .composer_mentions
                .iter()
                .find(|mention| mention.range.end == cursor)
                .map_or_else(
                    || self.previous_boundary(cursor),
                    |mention| mention.range.start,
                );
            self.move_to(target, cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &input::Right, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        if let Some(active) = self.active_page_number_edit.clone()
            && self.selected_range.is_empty()
            && self.cursor_offset() >= active.digits.end
        {
            self.finish_page_number_edit(cx);
            return;
        }
        if self.selected_range.is_empty() {
            let cursor = self.selected_range.end;
            let target = self
                .composer_mentions
                .iter()
                .find(|mention| mention.range.start == cursor)
                .map_or_else(|| self.next_boundary(cursor), |mention| mention.range.end);
            self.move_to(target, cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn up(&mut self, _: &input::Up, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        if self.select_previous_mention_completion(cx) {
            return;
        }
        self.move_vertically(-1, false, window, cx);
    }

    fn down(&mut self, _: &input::Down, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        if self.select_next_mention_completion(cx) {
            return;
        }
        self.move_vertically(1, false, window, cx);
    }

    fn home(&mut self, _: &input::Home, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        self.move_to(self.line_start_offset(), cx);
    }

    fn end(&mut self, _: &input::End, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        self.move_to(self.line_end_offset(), cx);
    }

    fn select_all(&mut self, _: &input::SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn select_left(&mut self, _: &input::SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(
        &mut self,
        _: &input::SelectRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_up(&mut self, _: &input::SelectUp, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        self.move_vertically(-1, true, window, cx);
    }

    fn select_down(&mut self, _: &input::SelectDown, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        self.move_vertically(1, true, window, cx);
    }

    fn select_home(&mut self, _: &input::SelectHome, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        self.select_to(self.line_start_offset(), cx);
    }

    fn select_end(&mut self, _: &input::SelectEnd, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        self.select_to(self.line_end_offset(), cx);
    }

    fn copy(&mut self, _: &input::Copy, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        self.copy_selected_text(cx);
    }

    fn cut(&mut self, _: &input::Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        if self.copy_selected_text(cx) {
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn paste(&mut self, _: &input::Paste, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            let editing_page_number = self.active_page_number_edit.is_some();
            self.replace_text_in_range(None, &text, window, cx);
            if !editing_page_number {
                self.rebuild_composer_mentions();
            }
            cx.notify();
        }
    }

    fn send_message(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }

        self.finish_page_number_edit(cx);

        let display_prompt = self.content.trim().to_string();
        if display_prompt.is_empty() {
            return;
        }
        let request_prompt = self.request_prompt();
        let references = self.references();

        let settings = cx.global::<SettingsStore>().settings.ai.resolved_main();
        self.update_current_page(cx);
        let include_document_text = references
            .iter()
            .any(|reference| reference.kind == MentionKind::DocumentText);
        let context = self.context_provider.context(include_document_text, cx);
        let contexts = self.context_blocks_for_references(&references, context.as_ref());
        let document = self.document_context_from(context);
        if self.mode == AiPanelMode::QuickAsk {
            self.messages.clear();
            self.markdown_views.clear();
        } else if self.messages.is_empty() {
            self.session_title = session_title_for_prompt(&display_prompt);
        }
        if self.mode == AiPanelMode::Sidebar {
            self.content = SharedString::default();
            self.selected_range = 0..0;
            self.composer_mentions.clear();
        } else {
            self.content = display_prompt.clone().into();
            self.selected_range = display_prompt.len()..display_prompt.len();
            self.rebuild_composer_mentions();
        }
        self.selection_reversed = false;
        self.marked_range = None;
        self.mention_completion = None;
        self.active_page_number_edit = None;
        self.last_cursor_pos = None;
        self.close_composer_menus();
        let user_prompt = display_prompt.clone();
        self.messages.push(ChatMessage::user(
            display_prompt,
            request_prompt,
            contexts,
            references,
        ));
        self.markdown_views
            .push(cx.new(|cx| Markdown::new(user_prompt.into(), cx)));
        self.messages.push(ChatMessage::assistant(String::new()));
        self.markdown_views
            .push(cx.new(|cx| Markdown::new(SharedString::default(), cx)));
        self.loading = true;
        cx.notify();

        let mut request_messages = self.messages[..self.messages.len().saturating_sub(1)].to_vec();
        let page_references = match unresolved_page_references(&request_messages) {
            Ok(references) => references,
            Err(error) => {
                self.apply_stream_event(AiStreamEvent::Error(error.to_string()), cx);
                return;
            }
        };
        let resolve_task = self
            .context_provider
            .resolve_page_references(page_references, cx);

        self.pending_stream = Some(cx.spawn(async move |this, cx| {
            let resolved = match resolve_task.await {
                Ok(resolved) => resolved,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.apply_stream_event(AiStreamEvent::Error(format!("{error:#}")), cx);
                        cx.notify();
                    });
                    return;
                }
            };
            if let Err(error) = validate_resolved_page_contexts(&request_messages, &resolved) {
                let _ = this.update(cx, |this, cx| {
                    this.apply_stream_event(AiStreamEvent::Error(error.to_string()), cx);
                    cx.notify();
                });
                return;
            }
            hydrate_page_contexts(&mut request_messages, &resolved);
            if this
                .update(cx, |this, cx| {
                    hydrate_page_contexts(&mut this.messages, &resolved);
                    cx.notify();
                })
                .is_err()
            {
                return;
            }

            let (tx, mut rx) = mpsc::unbounded::<AiStreamEvent>();
            thread::spawn(move || {
                let mut send_event =
                    |event| tx.unbounded_send(event).map_err(|_| anyhow!("closed"));
                let result =
                    stream_completion(settings, request_messages, document, &mut send_event);
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
                    message.push_text(&delta);
                }
                self.sync_last_markdown(cx);
            }
            AiStreamEvent::Error(error) => {
                self.loading = false;
                if let Some(message) = self.messages.last_mut() {
                    if !message.display_text().is_empty() {
                        message.push_text("\n\n");
                    }
                    message.push_text("Error: ");
                    message.push_text(&error);
                }
                self.sync_last_markdown(cx);
                self.pending_stream = None;
                if self.mode == AiPanelMode::Sidebar {
                    self.save_current_session(cx);
                }
            }
            AiStreamEvent::Done => {
                self.loading = false;
                self.sync_last_markdown(cx);
                self.pending_stream = None;
                if self.mode == AiPanelMode::Sidebar {
                    self.maybe_generate_title(cx);
                    self.save_current_session(cx);
                }
            }
        }
    }

    fn sync_last_markdown(&mut self, cx: &mut Context<Self>) {
        if let (Some(message), Some(md)) = (self.messages.last(), self.markdown_views.last()) {
            let text = message.display_text();
            md.update(cx, |md, cx| md.set_source(text.into(), cx));
        }
    }

    fn maybe_generate_title(&mut self, cx: &mut Context<Self>) {
        if self.mode == AiPanelMode::QuickAsk {
            return;
        }

        if self.pending_title_task.is_some() || !self.is_first_turn() {
            return;
        }

        let Some(settings) = cx.global::<SettingsStore>().settings.ai.resolved_title() else {
            return;
        };

        let session_id = self.session_id;
        let mut title_messages = self
            .messages
            .iter()
            .map(ChatMessage::title_safe)
            .collect::<Vec<_>>();
        title_messages.push(ChatMessage::user_text(TITLE_GENERATION_PROMPT.to_string()));
        let document = self.document_context(cx);

        let (tx, mut rx) = mpsc::unbounded::<AiStreamEvent>();

        thread::spawn(move || {
            let mut send_event = |event| tx.unbounded_send(event).map_err(|_| anyhow!("closed"));
            let result = stream_completion(settings, title_messages, document, &mut send_event);
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

    fn context_blocks_for_references(
        &self,
        references: &[MentionReference],
        context: Option<&AiContext>,
    ) -> Vec<ContextBlock> {
        references
            .iter()
            .filter(|reference| {
                matches!(
                    reference.kind,
                    MentionKind::Selection | MentionKind::DocumentText
                )
            })
            .map(|reference| self.resolve_mention(reference.kind, context))
            .collect()
    }

    fn resolve_mention(&self, mention: MentionKind, context: Option<&AiContext>) -> ContextBlock {
        let Some(context) = context else {
            return Self::missing_context_block(mention);
        };

        match mention {
            MentionKind::Selection => ContextBlock::new(
                mention,
                None,
                context
                    .selected_text
                    .clone()
                    .unwrap_or_else(|| tr("ai-no-selected-text")),
            ),
            MentionKind::DocumentText => ContextBlock::new(
                mention,
                None,
                context
                    .document_text
                    .clone()
                    .unwrap_or_else(|| tr("ai-no-document-text")),
            ),
            MentionKind::Page | MentionKind::PageImage => Self::missing_context_block(mention),
        }
    }

    fn missing_context_block(mention: MentionKind) -> ContextBlock {
        let message = match mention {
            MentionKind::Page => tr("ai-no-current-page-text"),
            MentionKind::PageImage => "No page image is available.".to_string(),
            MentionKind::Selection => tr("ai-no-selected-text"),
            MentionKind::DocumentText => tr("ai-no-document-text"),
        };
        ContextBlock::new(mention, None, message)
    }

    fn document_context(&self, cx: &mut Context<Self>) -> DocumentContext {
        self.document_context_from(self.context_provider.context(false, cx))
    }

    fn document_context_from(&self, context: Option<AiContext>) -> DocumentContext {
        let mut document = DocumentContext {
            file_name: self
                .document_path
                .file_name()
                .and_then(|file_name| file_name.to_str())
                .map(str::to_string),
            format: None,
            location_label: None,
            page_count: 0,
            current_page: self.current_page,
            has_selection: false,
        };

        if let Some(context) = context {
            document.file_name = context.document_file_name.or(document.file_name);
            document.format = context.document_format;
            document.location_label = context.location_label;
            document.page_count = context.page_count;
            document.current_page = context.current_page;
            document.has_selection = context.selected_text.is_some();
        }

        document
    }

    fn update_current_page(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.context_provider.page_state(cx) {
            self.current_page = state.current_page;
            self.page_count = state.page_count;
        }
    }

    fn rebuild_composer_mentions(&mut self) {
        self.composer_mentions = parse_composer_mentions(&self.content);
        self.active_page_number_edit = None;
    }

    fn references(&self) -> Vec<MentionReference> {
        let mut seen = HashSet::new();
        self.composer_mentions
            .iter()
            .filter_map(|mention| seen.insert(mention.reference).then_some(mention.reference))
            .collect()
    }

    fn request_prompt(&self) -> String {
        request_prompt_without_mentions(&self.content, &self.composer_mentions)
    }

    fn mention_at_offset(&self, offset: usize) -> Option<&ComposerMention> {
        self.composer_mentions
            .iter()
            .find(|mention| mention.range.start <= offset && offset <= mention.range.end)
    }

    fn page_digits_range(mention: &ComposerMention) -> Option<Range<usize>> {
        if !mention.reference.kind.requires_page_number() {
            return None;
        }
        let start = mention.range.start + mention.reference.kind.token().len() + 1;
        Some(start..mention.range.end.saturating_sub(1))
    }

    fn begin_page_number_edit(&mut self, mention_start: usize, cx: &mut Context<Self>) -> bool {
        let Some(mention) = self
            .composer_mentions
            .iter()
            .find(|mention| mention.range.start == mention_start)
        else {
            return false;
        };
        let Some(digits) = Self::page_digits_range(mention) else {
            return false;
        };
        self.selected_range = digits.clone();
        self.selection_reversed = false;
        self.active_page_number_edit = Some(ActivePageNumberEdit {
            mention_start,
            digits,
        });
        self.mention_completion = None;
        cx.notify();
        true
    }

    fn finish_page_number_edit(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(active) = self.active_page_number_edit.take() else {
            return false;
        };
        let page = validate_page_number(&self.content[active.digits.clone()], self.page_count);
        let page_text = page.to_string();
        let old_digits = active.digits.clone();
        self.content = (self.content[..old_digits.start].to_owned()
            + &page_text
            + &self.content[old_digits.end..])
            .into();
        let new_digits = old_digits.start..old_digits.start + page_text.len();
        let delta = page_text.len() as isize - old_digits.len() as isize;
        adjust_mentions_for_page_number_edit(
            &mut self.composer_mentions,
            active.mention_start,
            new_digits.clone(),
            delta,
            page,
        );
        let cursor = self
            .composer_mentions
            .iter()
            .find(|mention| mention.range.start == active.mention_start)
            .map_or(new_digits.end + 1, |mention| mention.range.end);
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        self.update_mention_completion();
        cx.notify();
        true
    }

    fn apply_regular_edit(&mut self, range: Range<usize>, new_text: &str) {
        self.content =
            (self.content[..range.start].to_owned() + new_text + &self.content[range.end..]).into();
        adjust_composer_mentions(&mut self.composer_mentions, &range, new_text.len());
        self.active_page_number_edit = None;
        let cursor = range.start + new_text.len();
        self.selected_range = cursor..cursor;
    }

    fn apply_page_number_edit(&mut self, range: Range<usize>, new_text: &str) -> bool {
        let Some(active) = self.active_page_number_edit.clone() else {
            return false;
        };
        if range.start < active.digits.start || range.end > active.digits.end {
            return false;
        }
        let filtered = new_text
            .bytes()
            .filter(u8::is_ascii_digit)
            .map(char::from)
            .collect::<String>();
        if !new_text.is_empty() && filtered.is_empty() {
            return true;
        }
        self.content =
            (self.content[..range.start].to_owned() + &filtered + &self.content[range.end..])
                .into();
        let delta = filtered.len() as isize - range.len() as isize;
        let new_digits = active.digits.start
            ..((active.digits.end as isize + delta).max(active.digits.start as isize) as usize);
        adjust_mentions_for_page_number_edit(
            &mut self.composer_mentions,
            active.mention_start,
            new_digits.clone(),
            delta,
            0,
        );
        self.active_page_number_edit = Some(ActivePageNumberEdit {
            mention_start: active.mention_start,
            digits: new_digits,
        });
        let cursor = range.start + filtered.len();
        self.selected_range = cursor..cursor;
        true
    }

    pub fn save_current_session(&mut self, cx: &mut Context<Self>) {
        if self.mode == AiPanelMode::QuickAsk {
            return;
        }

        if self.messages.iter().all(ChatMessage::is_display_empty) {
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
                .map(|message| {
                    let content = message.display_text();
                    let request_content = message.request_text();
                    AiSessionMessage {
                        role: match message.role {
                            Role::User => AiSessionRole::User,
                            Role::Assistant => AiSessionRole::Assistant,
                        },
                        content: content.clone(),
                        request_content: (request_content != content).then_some(request_content),
                        references: message
                            .references()
                            .iter()
                            .copied()
                            .map(ai_reference_to_library)
                            .collect(),
                    }
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
        self.close_composer_menus();
        cx.notify();
    }

    fn show_chat(&mut self, cx: &mut Context<Self>) {
        self.view = AiPanelView::Chat;
        cx.notify();
    }

    fn close_composer_menus(&mut self) {
        self.attachment_menu_open = false;
        self.attachment_menu_anchor = None;
        self.model_menu_open = false;
        self.model_menu_anchor = None;
        self.thinking_menu_open = false;
        self.thinking_menu_anchor = None;
        self.model_search_query = SharedString::default();
    }

    fn menu_anchor_for_button(position: GpuiPoint<Pixels>) -> GpuiPoint<Pixels> {
        point(position.x, position.y + px(18.0))
    }

    fn toggle_attachment_menu(&mut self, position: GpuiPoint<Pixels>, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.attachment_menu_open = !self.attachment_menu_open;
        self.attachment_menu_anchor = self
            .attachment_menu_open
            .then_some(Self::menu_anchor_for_button(position));
        self.model_menu_open = false;
        self.model_menu_anchor = None;
        self.thinking_menu_open = false;
        self.thinking_menu_anchor = None;
        cx.notify();
    }

    fn toggle_model_menu(
        &mut self,
        position: GpuiPoint<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading {
            return;
        }
        self.focus_handle.focus(window, cx);
        self.model_menu_open = !self.model_menu_open;
        self.model_menu_anchor = self
            .model_menu_open
            .then_some(Self::menu_anchor_for_button(position));
        self.attachment_menu_open = false;
        self.attachment_menu_anchor = None;
        self.thinking_menu_open = false;
        self.thinking_menu_anchor = None;
        self.model_search_query = SharedString::default();
        cx.notify();
    }

    fn toggle_thinking_menu(&mut self, position: GpuiPoint<Pixels>, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.thinking_menu_open = !self.thinking_menu_open;
        self.thinking_menu_anchor = self
            .thinking_menu_open
            .then_some(Self::menu_anchor_for_button(position));
        self.attachment_menu_open = false;
        self.attachment_menu_anchor = None;
        self.model_menu_open = false;
        self.model_menu_anchor = None;
        cx.notify();
    }

    fn select_model(&mut self, provider: AiProvider, model: String, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }

        let mut settings = cx.global::<SettingsStore>().settings.clone();
        settings.ai.main_model = Some(ModelRef::new(provider, model.clone()));
        settings.ai.thinking_level = settings.ai.thinking_level.clamp_for(provider, &model);
        if let Err(error) = cx.global_mut::<SettingsStore>().set_settings(settings) {
            eprintln!("failed to update AI model: {error:#}");
        }
        self.close_composer_menus();
        cx.notify();
    }

    fn select_thinking_level(&mut self, level: AiThinkingLevel, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        let main = settings.ai.resolved_main();
        let provider = main.provider;
        let model = main.model;
        settings.ai.thinking_level = level.clamp_for(provider, &model);
        if let Err(error) = cx.global_mut::<SettingsStore>().set_settings(settings) {
            eprintln!("failed to update AI thinking level: {error:#}");
        }
        self.close_composer_menus();
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
        self.markdown_views.clear();
        self.content = SharedString::default();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.mention_completion = None;
        self.composer_mentions.clear();
        self.active_page_number_edit = None;
        self.last_cursor_pos = None;
        self.pending_title_task = None;
        self.close_composer_menus();
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
            .map(|message| {
                let role = match message.role {
                    AiSessionRole::User => Role::User,
                    AiSessionRole::Assistant => Role::Assistant,
                };
                ChatMessage::from_persisted(role, message.content, message.request_content)
                    .with_references(
                        message
                            .references
                            .into_iter()
                            .map(library_reference_to_ai)
                            .collect(),
                    )
            })
            .collect();
        self.markdown_views = self
            .messages
            .iter()
            .map(|message| {
                let source = message.display_text();
                cx.new(|cx| Markdown::new(source.into(), cx))
            })
            .collect();
        self.content = SharedString::default();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.mention_completion = None;
        self.composer_mentions.clear();
        self.active_page_number_edit = None;
        self.last_cursor_pos = None;
        self.pending_title_task = None;
        self.close_composer_menus();
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
        active_mention_range(&self.content, cursor)
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
            .filter(|mention| self.page_count > 0 || !mention.requires_page_number())
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

    fn add_attachment(
        &mut self,
        mention: MentionKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading {
            return;
        }
        self.focus_handle.focus(window, cx);
        self.close_composer_menus();
        if mention.requires_page_number() && self.page_count == 0 {
            cx.notify();
            return;
        }
        if self
            .composer_mentions
            .iter()
            .any(|composer_mention| composer_mention.reference.kind == mention)
            && !mention.requires_page_number()
        {
            cx.notify();
            return;
        }

        let cursor = self.cursor_offset().min(self.content.len());
        self.insert_mention_at_range(mention, cursor..cursor, cx);
    }

    fn insert_mention_at_range(
        &mut self,
        mention: MentionKind,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) {
        if mention.requires_page_number() {
            self.update_current_page(cx);
        }
        if mention.requires_page_number() && self.page_count == 0 {
            self.mention_completion = None;
            cx.notify();
            return;
        }
        let page = self.current_page.clamp(1, self.page_count.max(1));
        let token = if mention.requires_page_number() {
            MentionReference::new(mention, Some(page)).display_text()
        } else {
            mention.token().to_string()
        };
        let needs_leading_space = range.start > 0
            && self.content[..range.start]
                .chars()
                .last()
                .is_some_and(|ch| !ch.is_whitespace());
        let needs_trailing_space = self.content[range.end..]
            .chars()
            .next()
            .is_none_or(|ch| !ch.is_whitespace());
        let mut new_text = String::new();
        if needs_leading_space {
            new_text.push(' ');
        }
        let mention_start = range.start + new_text.len();
        new_text.push_str(&token);
        let mention_end = range.start + new_text.len();
        if needs_trailing_space {
            new_text.push(' ');
        }
        let insertion_start = range.start;
        self.apply_regular_edit(range, &new_text);
        let reference =
            MentionReference::new(mention, mention.requires_page_number().then_some(page));
        self.composer_mentions.push(ComposerMention {
            reference,
            range: mention_start..mention_end,
        });
        self.composer_mentions
            .sort_by_key(|composer_mention| composer_mention.range.start);
        if mention.requires_page_number() {
            self.begin_page_number_edit(mention_start, cx);
        } else {
            let cursor = insertion_start + new_text.len();
            self.selected_range = cursor..cursor;
        }
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
        self.close_composer_menus();
        self.drag_origin = Some(event.position);

        let mut offset = self.index_for_mouse_position(event.position, window);
        if self
            .active_page_number_edit
            .as_ref()
            .is_some_and(|active| offset < active.digits.start || offset > active.digits.end)
            && self.finish_page_number_edit(cx)
        {
            offset = self.index_for_mouse_position(event.position, window);
        }
        if !event.modifiers.shift
            && let Some(mention) = self.mention_at_offset(offset).cloned()
        {
            if let Some(digits) = Self::page_digits_range(&mention)
                && digits.start <= offset
                && offset <= digits.end
            {
                self.begin_page_number_edit(mention.range.start, cx);
                self.selected_range = offset..offset;
                cx.notify();
                cx.stop_propagation();
                return;
            }
            let midpoint = mention.range.start + mention.range.len() / 2;
            self.move_to(
                if offset < midpoint {
                    mention.range.start
                } else {
                    mention.range.end
                },
                cx,
            );
            cx.stop_propagation();
            return;
        }
        if event.modifiers.shift {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
        cx.stop_propagation();
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.drag_origin = None;
        cx.stop_propagation();
    }

    fn on_mouse_up_out(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.drag_origin = None;
    }

    fn on_mouse_down_out(&mut self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.cancel_selection(cx);
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            self.drag_origin = None;
            return;
        }

        let should_select = if let Some(origin) = self.drag_origin.as_ref() {
            (event.position.x - origin.x).as_f32().abs() > DRAG_SELECT_THRESHOLD
                || (event.position.y - origin.y).as_f32().abs() > DRAG_SELECT_THRESHOLD
        } else {
            false
        };

        if should_select {
            self.select_to(self.index_for_mouse_position(event.position, window), cx);
            cx.stop_propagation();
        }
    }

    fn cancel_selection(&mut self, cx: &mut Context<Self>) {
        self.drag_origin = None;
        if !self.selected_range.is_empty() {
            self.move_to(self.cursor_offset(), cx);
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

    fn render_quick_ask(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity();
        let (theme, quick_ask, base_text_style) = {
            let store = cx.global::<SettingsStore>();
            let font = &store.settings.font;
            let base_text_style = TextStyle {
                font_family: font.ui_font_family.clone().into(),
                font_size: px(store.settings.ai.quick_ask.text_size).into(),
                color: rgb(store.resolved_theme.text).into(),
                font_weight: FontWeight(font.ui_font_weight),
                ..Default::default()
            };
            (
                store.resolved_theme,
                store.settings.ai.quick_ask,
                base_text_style,
            )
        };
        let loading = self.loading;
        let completion_matches = self.mention_completion_matches();
        let completion_selected = self
            .mention_completion
            .as_ref()
            .map(|completion| completion.selected_ix)
            .unwrap_or(0);
        let completion_anchor = if completion_matches.is_empty() {
            None
        } else {
            self.last_bounds
                .zip(self.last_cursor_pos)
                .map(|(bounds, position)| {
                    let max_left = px((self.width - COMPLETION_MENU_WIDTH - 24.0).max(0.0));
                    let left = if position.x < px(0.0) {
                        px(0.0)
                    } else if position.x > max_left {
                        max_left
                    } else {
                        position.x
                    };
                    point(
                        bounds.origin.x + left,
                        bounds.origin.y + position.y - px(8.0),
                    )
                })
        };
        let answer = self
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::Assistant)
            .map(ChatMessage::display_text)
            .unwrap_or_default();
        let answer_md = self
            .messages
            .iter()
            .rposition(|m| m.role == Role::Assistant)
            .and_then(|ix| self.markdown_views.get(ix).cloned());
        let show_answer = loading || !answer.is_empty();

        div()
            .key_context("QuickAsk TextInput")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .w_full()
            .border_t(px(1.0))
            .border_color(rgb(theme.border))
            .bg(rgba(with_alpha(
                theme.surface_background,
                quick_ask.background_alpha(),
            )))
            .text_size(px(quick_ask.text_size))
            .text_color(rgb(theme.text))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
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
                div().flex().flex_col().child(
                    div()
                        .relative()
                        .key_context("TextInput")
                        .min_h(px(32.0))
                        .px_3()
                        .py_2()
                        .cursor(CursorStyle::IBeam)
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                        .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                        .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up_out))
                        .on_mouse_down_out(cx.listener(Self::on_mouse_down_out))
                        .on_mouse_move(cx.listener(Self::on_mouse_move))
                        .child(AiInputElement { panel: entity }),
                ),
            )
            .when(show_answer, |this| {
                this.child(
                    div()
                        .border_t(px(1.0))
                        .border_color(rgb(theme.border_variant))
                        .child(
                            div()
                                .id("quick-ask-answer")
                                .track_scroll(&self.scroll_handle)
                                .max_h(px(320.0))
                                .p_3()
                                .flex()
                                .flex_col()
                                .overflow_y_scroll()
                                .text_color(rgb(theme.text))
                                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                                .when(answer.is_empty(), |this| {
                                    this.child(Self::render_loading_indicator(theme))
                                })
                                .when(!answer.is_empty(), |this| {
                                    this.when_some(answer_md, |this, md| {
                                        this.child(MarkdownElement::new(
                                            md,
                                            MarkdownStyle::from_theme(
                                                theme,
                                                base_text_style.clone(),
                                            ),
                                        ))
                                    })
                                }),
                        )
                        .vertical_scrollbar_for(&self.scroll_handle, window, cx),
                )
            })
            .when_some(completion_anchor, |this, anchor| {
                this.child(
                    deferred(
                        anchored()
                            .position(anchor)
                            .anchor(Anchor::BottomLeft)
                            .snap_to_window_with_margin(px(8.0))
                            .child(Self::render_mention_completion_menu(
                                completion_matches,
                                completion_selected,
                                theme,
                                cx,
                            )),
                    )
                    .with_priority(1),
                )
            })
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

    fn render_message(
        message: ChatMessage,
        markdown: Entity<Markdown>,
        theme: Theme,
        panel_width: f32,
        base_text_style: TextStyle,
    ) -> impl IntoElement {
        let is_user = message.role == Role::User;
        let label = if is_user { "You" } else { "Assistant" };
        let content = message.display_text();
        let is_loading = content.is_empty();
        let row_width = (panel_width - 24.0).max(160.0);
        let max_width = (row_width - 24.0).max(160.0);

        div()
            .w(px(row_width))
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .when(is_user, |this| this.justify_end())
                    .text_size(ui_rems(11.0))
                    .text_color(rgb(theme.text_muted))
                    .child(label),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .when(is_user, |this| this.justify_end())
                    .child(
                        div()
                            .max_w(px(max_width))
                            .when(is_user, |this| {
                                this.border_1()
                                    .border_color(rgba(with_alpha(theme.accent, 0x55)))
                                    .bg(rgba(with_alpha(theme.accent, 0x22)))
                                    .px_2()
                                    .py_1()
                            })
                            .when(!is_user, |this| this.px_1().py_1())
                            .text_size(ui_rems(13.0))
                            .text_color(rgb(theme.text))
                            .when(is_loading, |this| {
                                this.child(Self::render_loading_indicator(theme))
                            })
                            .when(!is_loading, |this| {
                                this.child(MarkdownElement::new(
                                    markdown,
                                    MarkdownStyle::from_theme(theme, base_text_style),
                                ))
                            }),
                    ),
            )
    }

    fn render_loading_indicator(theme: Theme) -> impl IntoElement {
        let dot = move |opacity| {
            div()
                .w(px(4.0))
                .h(px(4.0))
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
        theme: Theme,
        scroll_handle: &ScrollHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut list = div().flex().flex_col().gap_1().p_2();
        list = list.child(
            div()
                .id("ai-new-session")
                .flex()
                .items_center()
                .justify_between()
                .border_1()
                .border_color(rgb(theme.accent))
                .bg(rgba(with_alpha(theme.accent, 0x18)))
                .px_2()
                .py_2()
                .text_size(ui_rems(13.0))
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
                .child(tr("ai-new-chat"))
                .child("+"),
        );

        if sessions.is_empty() {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(120.0))
                    .text_size(ui_rems(13.0))
                    .text_color(rgb(theme.text_muted))
                    .child(tr("ai-no-previous-sessions")),
            );
        }

        for session in sessions {
            let active = session.id == current_session_id;
            let session_id = session.id;
            let title = session.title.clone();
            let meta = format!("p. {} · {} messages", session.page, session.messages.len());
            list = list.child(
                div()
                    .id(format!("ai-session-{session_id}"))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .py_2()
                    .bg(if active {
                        rgb(theme.element_selected)
                    } else {
                        rgba(0x00000000)
                    })
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(theme.element_hover)))
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
                            .text_size(ui_rems(13.0))
                            .text_color(rgb(if active { theme.accent } else { theme.text }))
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(ui_rems(11.0))
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
            .track_scroll(scroll_handle)
            .child(list)
            .vertical_scrollbar_for(scroll_handle, window, cx)
    }

    fn render_mention_completion_menu(
        matches: Vec<MentionKind>,
        selected_ix: usize,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut menu = div()
            .occlude()
            .w(px(COMPLETION_MENU_WIDTH))
            .flex()
            .flex_col()
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .shadow_sm()
            .overflow_hidden()
            .text_size(ui_rems(12.0));

        for (ix, mention) in matches.into_iter().enumerate() {
            let selected = ix == selected_ix;
            menu = menu.child(
                div()
                    .id(("ai-mention-completion", ix))
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(COMPLETION_MENU_ROW_HEIGHT))
                    .mx_1()
                    .px_2()
                    .rounded(px(4.0))
                    .bg(if selected {
                        rgb(theme.element_selected)
                    } else {
                        rgba(0x00000000)
                    })
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(theme.element_hover)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.insert_mention(mention, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .text_color(rgb(if selected {
                                theme.accent
                            } else {
                                theme.text_muted
                            }))
                            .child("@"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_color(rgb(if selected { theme.accent } else { theme.text }))
                            .child(mention.label()),
                    )
                    .child(
                        div()
                            .text_size(ui_rems(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(mention.description()),
                    ),
            );
        }

        menu
    }

    fn render_attachment_menu(
        page_count: usize,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut menu = div()
            .w(px(260.0))
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .shadow_sm()
            .overflow_hidden()
            .text_size(ui_rems(12.0))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            });

        for (mention_ix, mention) in MentionKind::ALL
            .into_iter()
            .filter(|mention| page_count > 0 || !mention.requires_page_number())
            .enumerate()
        {
            menu = menu.child(
                div()
                    .id(("ai-attachment-menu-entry", mention_ix))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_2()
                    .py_2()
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(theme.element_hover)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.add_attachment(mention, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_color(rgb(theme.text)).child(mention.label()))
                            .child(
                                div()
                                    .text_size(ui_rems(11.0))
                                    .text_color(rgb(theme.text_muted))
                                    .child(mention.description()),
                            ),
                    )
                    .child(
                        div()
                            .text_size(ui_rems(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(mention.token()),
                    ),
            );
        }

        menu
    }

    fn render_model_menu(
        state: ModelMenuRenderState,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let query = state.search_query.to_string();
        let query_empty = query.is_empty();
        let query_lower = query.to_ascii_lowercase();
        let mut entries = Vec::new();
        for (provider, models) in state.provider_models {
            let mut provider_entries = models
                .into_iter()
                .filter(|model| {
                    query_lower.is_empty()
                        || model.id.to_ascii_lowercase().contains(&query_lower)
                        || model.name.to_ascii_lowercase().contains(&query_lower)
                        || model.group.to_ascii_lowercase().contains(&query_lower)
                        || provider.label().to_ascii_lowercase().contains(&query_lower)
                })
                .map(move |model| ModelMenuEntry::Model(provider, model))
                .collect::<Vec<_>>();
            if !provider_entries.is_empty() {
                entries.push(ModelMenuEntry::Header(provider));
                entries.append(&mut provider_entries);
            }
        }
        let entry_count = entries.len();
        let scroll_handle = state.scroll_handle.clone();
        let search_focused = state.focused;

        let mut menu = div()
            .w(px(340.0))
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .shadow_sm()
            .overflow_hidden()
            .text_size(ui_rems(12.0))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            });

        menu = menu
            .child(
                div()
                    .mx_2()
                    .mt_2()
                    .mb_2()
                    .h(px(30.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .border_1()
                    .border_color(rgb(if search_focused {
                        theme.accent
                    } else {
                        theme.border_variant
                    }))
                    .bg(rgb(theme.tab_active_background))
                    .text_color(rgb(if query_empty {
                        theme.text_muted
                    } else {
                        theme.text
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.focus_handle.focus(window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .when(query_empty && search_focused, |this| {
                        this.child(div().w(px(1.0)).h(px(14.0)).bg(rgb(theme.accent)))
                    })
                    .child(if query_empty {
                        tr("ai-search-models-placeholder")
                    } else {
                        query
                    })
                    .when(!query_empty && search_focused, |this| {
                        this.child(div().w(px(1.0)).h(px(14.0)).bg(rgb(theme.accent)))
                    }),
            )
            .child(
                div()
                    .mx_2()
                    .mb_1()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_size(ui_rems(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(tr("ai-select-provider-model")),
                    ),
            );

        if entries.is_empty() {
            menu = menu.child(
                div()
                    .px_2()
                    .py_2()
                    .text_color(rgb(theme.text_muted))
                    .child(tr("no-matching-models")),
            );
        }

        if entry_count > 0 {
            let selected_provider = state.selected_provider;
            let selected_model = state.selected_model;
            let rows = entries
                .into_iter()
                .map(|entry| match entry {
                    ModelMenuEntry::Header(provider) => div()
                        .h(px(26.0))
                        .flex()
                        .items_center()
                        .px_2()
                        .text_size(ui_rems(11.0))
                        .text_color(rgb(theme.text_muted))
                        .child(provider.label())
                        .into_any_element(),
                    ModelMenuEntry::Model(provider, model) => {
                        let selected = provider == selected_provider && model.id == selected_model;
                        let model_for_click = model.id.clone();
                        div()
                            .id(format!("ai-model-{provider:?}-{}", model.id))
                            .h(px(30.0))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .pl(px(20.0))
                            .pr_2()
                            .rounded(px(4.0))
                            .bg(if selected {
                                rgb(theme.element_selected)
                            } else {
                                rgba(0x00000000)
                            })
                            .text_color(rgb(if selected { theme.accent } else { theme.text }))
                            .cursor_pointer()
                            .hover(move |style| style.bg(rgb(theme.element_hover)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.select_model(provider, model_for_click.clone(), cx);
                                    cx.stop_propagation();
                                }),
                            )
                            .child(div().truncate().min_w_0().child(model.name))
                            .child(
                                div()
                                    .flex_none()
                                    .max_w(px(96.0))
                                    .truncate()
                                    .text_size(ui_rems(11.0))
                                    .text_color(rgb(theme.text_muted))
                                    .child(model.group),
                            )
                            .into_any_element()
                    }
                })
                .collect::<Vec<_>>();
            menu = menu.child(
                div()
                    .id("ai-model-menu-list")
                    .track_scroll(&scroll_handle)
                    .max_h(px(300.0))
                    .pb_1()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                    .children(rows)
                    .vertical_scrollbar_for(&scroll_handle, window, cx),
            );
        }

        menu
    }

    fn render_thinking_menu(
        selected_provider: AiProvider,
        selected_model: String,
        selected_level: AiThinkingLevel,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut menu = div()
            .w(px(280.0))
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .shadow_sm()
            .overflow_hidden()
            .text_size(ui_rems(12.0))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            });

        for (level_ix, level) in AiThinkingLevel::supported_for(selected_provider, &selected_model)
            .iter()
            .enumerate()
        {
            let level = *level;
            let selected = level == selected_level;
            menu = menu.child(
                div()
                    .id(("ai-thinking-level", level_ix))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_2()
                    .py_2()
                    .bg(if selected {
                        rgb(theme.element_selected)
                    } else {
                        rgba(0x00000000)
                    })
                    .text_color(rgb(if selected { theme.accent } else { theme.text }))
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(theme.element_hover)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.select_thinking_level(level, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(level.label())
                    .child(
                        div()
                            .text_size(ui_rems(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(thinking_level_description(level)),
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
        if self.model_menu_open {
            if !new_text.is_empty() {
                let mut query = self.model_search_query.to_string();
                query.push_str(new_text);
                self.model_search_query = query.into();
                cx.notify();
            }
            return;
        }
        if self.active_page_number_edit.is_some()
            && !new_text.is_empty()
            && !new_text.bytes().any(|byte| byte.is_ascii_digit())
        {
            return;
        }

        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        if !self.apply_page_number_edit(range.clone(), new_text) {
            self.apply_regular_edit(range, new_text);
        }
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
        if self.model_menu_open {
            if !new_text.is_empty() {
                let mut query = self.model_search_query.to_string();
                query.push_str(new_text);
                self.model_search_query = query.into();
                cx.notify();
            }
            return;
        }
        if self.active_page_number_edit.is_some()
            && !new_text.is_empty()
            && !new_text.bytes().any(|byte| byte.is_ascii_digit())
        {
            return;
        }

        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let selected_range_start = range.start;

        let inserted_len = if self.active_page_number_edit.is_some() {
            new_text.bytes().filter(u8::is_ascii_digit).count()
        } else {
            new_text.len()
        };
        if !self.apply_page_number_edit(range.clone(), new_text) {
            self.apply_regular_edit(range.clone(), new_text);
        }
        self.marked_range =
            (!new_text.is_empty()).then_some(range.start..range.start + inserted_len);
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| {
                let start = offset_from_utf16_in_text(new_text, range_utf16.start);
                let end = offset_from_utf16_in_text(new_text, range_utf16.end);
                selected_range_start + start..selected_range_start + end
            })
            .unwrap_or_else(|| range.start + inserted_len..range.start + inserted_len);
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

impl EventEmitter<AiPanelEvent> for AiPanel {}

impl Focusable for AiPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AiPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.mode == AiPanelMode::QuickAsk {
            return self.render_quick_ask(window, cx).into_any_element();
        }

        let entity = cx.entity();
        let (theme, ai_settings, base_text_style) = {
            let store = cx.global::<SettingsStore>();
            let font = &store.settings.font;
            let base_text_style = TextStyle {
                font_family: font.ui_font_family.clone().into(),
                font_size: ui_rems(13.0).into(),
                color: rgb(store.resolved_theme.text).into(),
                font_weight: FontWeight(font.ui_font_weight),
                ..Default::default()
            };
            (
                store.resolved_theme,
                store.settings.ai.clone(),
                base_text_style,
            )
        };
        let can_send = !self.loading && !self.content.trim().is_empty();
        let resolved_main = ai_settings.resolved_main();
        let selected_provider = resolved_main.provider;
        let selected_model = resolved_main.model;
        let selected_thinking_level = ai_settings
            .thinking_level
            .clamp_for(selected_provider, &selected_model);
        let model_menu_selected_model = selected_model.clone();
        let thinking_menu_selected_model = selected_model.clone();
        let provider_models = AiProvider::ALL
            .into_iter()
            .filter_map(|provider| {
                let config = ai_settings.provider_config(provider);
                (config.enabled && !config.models.is_empty()).then_some((provider, config.models))
            })
            .collect::<Vec<_>>();
        let model_label = ai_settings
            .main_model
            .as_ref()
            .map(|model_ref| ai_settings.model_label(model_ref))
            .unwrap_or_else(|| selected_model.clone());
        let attachment_menu_open = self.attachment_menu_open;
        let attachment_menu_anchor = self.attachment_menu_anchor;
        let attachment_menu_position = attachment_menu_open
            .then_some(())
            .and(attachment_menu_anchor);
        let model_menu_open = self.model_menu_open;
        let model_menu_anchor = self.model_menu_anchor;
        let model_menu_position = model_menu_open.then_some(()).and(model_menu_anchor);
        let model_search_query = self.model_search_query.clone();
        let model_menu_scroll = self.model_menu_scroll.clone();
        let thinking_menu_open = self.thinking_menu_open;
        let thinking_menu_anchor = self.thinking_menu_anchor;
        let thinking_menu_position = thinking_menu_open.then_some(()).and(thinking_menu_anchor);
        let model_search_focused = model_menu_open && self.focus_handle.is_focused(window);
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
        let empty_chat = self.messages.is_empty();
        let panel_width = self.width;
        let completion_anchor = if showing_sessions || completion_matches.is_empty() {
            None
        } else {
            self.last_bounds
                .zip(self.last_cursor_pos)
                .map(|(bounds, position)| {
                    let max_left = px((self.width - COMPLETION_MENU_WIDTH - 24.0).max(0.0));
                    let left = if position.x < px(0.0) {
                        px(0.0)
                    } else if position.x > max_left {
                        max_left
                    } else {
                        position.x
                    };
                    point(
                        bounds.origin.x + left,
                        bounds.origin.y + position.y - px(8.0),
                    )
                })
        };

        let mut messages = div()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .w(px((panel_width - 1.0).max(0.0)));
        for (message, markdown) in self
            .messages
            .iter()
            .zip(self.markdown_views.iter())
            .map(|(m, md)| (m.clone(), md.clone()))
        {
            messages = messages.child(Self::render_message(
                message,
                markdown,
                theme,
                panel_width,
                base_text_style.clone(),
            ));
        }

        div()
            .key_context("AiPanel TextInput")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.width))
            .h_full()
            .border_l(px(1.0))
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
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
                    .border_color(rgb(theme.border))
                    .text_size(ui_rems(12.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(ui_rems(10.0))
                                    .text_color(rgb(theme.text_muted))
                                    .child(tr("ai-title")),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(ui_rems(13.0))
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
                                    .id("ai-panel-new-session")
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .w(px(24.0))
                                    .h(px(24.0))
                                    .text_size(ui_rems(14.0))
                                    .text_color(if loading {
                                        rgb(theme.text_muted)
                                    } else {
                                        rgb(theme.text)
                                    })
                                    .cursor_pointer()
                                    .hover(move |style| style.bg(rgb(theme.element_hover)))
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
                                    .id("ai-panel-toggle-sessions")
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .w(px(24.0))
                                    .h(px(24.0))
                                    .text_size(ui_rems(14.0))
                                    .text_color(rgb(if showing_sessions {
                                        theme.accent
                                    } else {
                                        theme.text_muted
                                    }))
                                    .cursor_pointer()
                                    .hover(move |style| style.bg(rgb(theme.element_hover)))
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
                                    .id("ai-panel-close")
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .w(px(24.0))
                                    .h(px(24.0))
                                    .text_size(ui_rems(14.0))
                                    .text_color(rgb(theme.text_muted))
                                    .cursor_pointer()
                                    .hover(move |style| style.bg(rgb(theme.element_hover)))
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
                    &self.sessions_scroll,
                    window,
                    cx,
                ))
            })
            .when(!showing_sessions, |this| {
                this.when(!empty_chat, |this| {
                    this.child(
                        div()
                            .id("ai-panel-messages")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_handle)
                            .child(messages)
                            .vertical_scrollbar_for(&self.scroll_handle, window, cx),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .when(empty_chat, |this| this.flex_1().min_h_0())
                        .gap_1()
                        .p_2()
                        .border_t(px(1.0))
                        .border_color(rgb(theme.border))
                        .bg(rgb(theme.surface_background))
                        .child(
                            div()
                                .relative()
                                .key_context("TextInput")
                                .when(empty_chat, |this| this.flex_1().min_h_0())
                                .rounded(px(6.0))
                                .border_1()
                                .border_color(rgb(theme.border_variant))
                                .bg(rgb(theme.tab_active_background))
                                .px_2()
                                .pt_2()
                                .pb_1()
                                .cursor(CursorStyle::IBeam)
                                .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                                .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                                .on_mouse_up_out(
                                    MouseButton::Left,
                                    cx.listener(Self::on_mouse_up_out),
                                )
                                .on_mouse_down_out(cx.listener(Self::on_mouse_down_out))
                                .on_mouse_move(cx.listener(Self::on_mouse_move))
                                .child(AiInputElement { panel: entity }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .rounded(px(6.0))
                                .border_1()
                                .border_color(rgb(theme.border_variant))
                                .bg(rgb(theme.tab_active_background))
                                .px_2()
                                .pb_2()
                                .pt_1()
                                .text_size(ui_rems(11.0))
                                .text_color(rgb(theme.text_muted))
                                .child(
                                    div()
                                        .flex()
                                        .flex_1()
                                        .min_w_0()
                                        .flex_wrap()
                                        .items_center()
                                        .gap_1()
                                        .child(
                                            div()
                                                .id("ai-attachment-menu-button")
                                                .flex()
                                                .items_center()
                                                .gap_1()
                                                .px_2()
                                                .py_1()
                                                .rounded(px(4.0))
                                                .text_size(ui_rems(11.0))
                                                .text_color(rgb(if attachment_menu_open {
                                                    theme.accent
                                                } else {
                                                    theme.text_muted
                                                }))
                                                .cursor_pointer()
                                                .hover(move |style| {
                                                    style.bg(rgb(theme.element_hover))
                                                })
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(
                                                        |this,
                                                         event: &MouseDownEvent,
                                                         _,
                                                         cx| {
                                                        this.toggle_attachment_menu(
                                                            event.position,
                                                            cx,
                                                        );
                                                        cx.stop_propagation();
                                                    }),
                                                )
                                                .child("+")
                                                .child(tr("attach")),
                                        )
                                        .child(
                                            div()
                                                .id("ai-model-menu-button")
                                                .flex()
                                                .items_center()
                                                .gap_1()
                                                .px_2()
                                                .py_1()
                                                .rounded(px(4.0))
                                                .text_color(rgb(if model_menu_open {
                                                    theme.accent
                                                } else {
                                                    theme.text_muted
                                                }))
                                                .cursor_pointer()
                                                .hover(move |style| {
                                                    style.bg(rgb(theme.element_hover))
                                                })
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(
                                                        |this,
                                                         event: &MouseDownEvent,
                                                         window,
                                                         cx| {
                                                        this.toggle_model_menu(
                                                            event.position,
                                                            window,
                                                            cx,
                                                        );
                                                        cx.stop_propagation();
                                                    }),
                                                )
                                                .child(
                                                    div()
                                                        .max_w(px(96.0))
                                                        .truncate()
                                                        .child(model_label),
                                                )
                                                .child(if model_menu_open { "⌃" } else { "⌄" }),
                                        )
                                        .child(
                                            div()
                                                .id("ai-thinking-menu-button")
                                                .flex()
                                                .items_center()
                                                .gap_1()
                                                .px_2()
                                                .py_1()
                                                .rounded(px(4.0))
                                                .text_color(rgb(
                                                    if thinking_menu_open
                                                        || selected_thinking_level
                                                            != AiThinkingLevel::Off
                                                    {
                                                        theme.accent
                                                    } else {
                                                        theme.text_muted
                                                    },
                                                ))
                                                .cursor_pointer()
                                                .hover(move |style| {
                                                    style.bg(rgb(theme.element_hover))
                                                })
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(
                                                        |this,
                                                         event: &MouseDownEvent,
                                                         _,
                                                         cx| {
                                                        this.toggle_thinking_menu(
                                                            event.position,
                                                            cx,
                                                        );
                                                        cx.stop_propagation();
                                                    }),
                                                )
                                                .child(format!(
                                                    "Think {}",
                                                    selected_thinking_level.label()
                                                ))
                                                .child(if thinking_menu_open {
                                                    "⌃"
                                                } else {
                                                    "⌄"
                                                }),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .rounded(px(4.0))
                                        .px_3()
                                        .py_1()
                                        .border_1()
                                        .border_color(if can_send {
                                            rgb(theme.accent)
                                        } else {
                                            rgb(theme.border_variant)
                                        })
                                        .bg(if can_send {
                                            rgba(with_alpha(theme.accent, 0x22))
                                        } else {
                                            rgb(theme.surface_background)
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
                                        .child(if loading {
                                            tr("sending")
                                        } else {
                                            tr("send")
                                        }),
                                ),
                        ),
                )
            })
            .when_some(completion_anchor, |this, anchor| {
                this.child(
                    deferred(
                        anchored()
                            .position(anchor)
                            .anchor(Anchor::BottomLeft)
                            .snap_to_window_with_margin(px(8.0))
                            .child(Self::render_mention_completion_menu(
                                completion_matches,
                                completion_selected,
                                theme,
                                cx,
                            )),
                    )
                    .with_priority(1),
                )
            })
            .when_some(attachment_menu_position, |this, anchor| {
                this.child(
                    deferred(
                        anchored()
                            .position(anchor)
                            .anchor(Anchor::TopLeft)
                            .snap_to_window_with_margin(px(8.0))
                            .child(Self::render_attachment_menu(self.page_count, theme, cx)),
                    )
                    .with_priority(2),
                )
            })
            .when_some(model_menu_position, |this, anchor| {
                this.child(
                    deferred(
                        anchored()
                            .position(anchor)
                            .anchor(Anchor::TopLeft)
                            .snap_to_window_with_margin(px(8.0))
                            .child(Self::render_model_menu(
                                ModelMenuRenderState {
                                    selected_provider,
                                    selected_model: model_menu_selected_model,
                                    provider_models,
                                    search_query: model_search_query,
                                    scroll_handle: model_menu_scroll,
                                    focused: model_search_focused,
                                },
                                theme,
                                window,
                                cx,
                            )),
                    )
                    .with_priority(2),
                )
            })
            .when_some(thinking_menu_position, |this, anchor| {
                this.child(
                    deferred(
                        anchored()
                            .position(anchor)
                            .anchor(Anchor::TopLeft)
                            .snap_to_window_with_margin(px(8.0))
                            .child(Self::render_thinking_menu(
                                selected_provider,
                                thinking_menu_selected_model,
                                selected_thinking_level,
                                theme,
                                cx,
                            )),
                    )
                    .with_priority(2),
                )
            })
            .into_any_element()
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
        let input = self.panel.read(cx);
        let content = input.content.clone();
        let empty_chat = input.mode == AiPanelMode::Sidebar && input.messages.is_empty();
        let chars_per_line = ((input.width - 32.0) / 7.0).floor().max(12.0) as usize;
        let text = content.as_ref();
        let min_lines = if input.mode == AiPanelMode::QuickAsk {
            1
        } else {
            3
        };
        let max_lines = if input.mode == AiPanelMode::QuickAsk {
            4
        } else {
            8
        };
        let estimated_lines = if text.is_empty() {
            if input.mode == AiPanelMode::QuickAsk || empty_chat {
                1
            } else {
                3
            }
        } else {
            text.lines()
                .map(|line| line.chars().count().saturating_sub(1) / chars_per_line + 1)
                .sum::<usize>()
                + usize::from(text.ends_with('\n'))
        }
        .clamp(min_lines, max_lines);

        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = if empty_chat {
            relative(1.0).into()
        } else {
            (window.line_height() * estimated_lines as f32).into()
        };
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
        let composer_mentions = input.composer_mentions.clone();
        let active_page_number_edit = input.active_page_number_edit.clone();
        let style = window.text_style();
        let theme = cx.global::<SettingsStore>().resolved_theme;
        let (display_text, text_color) = if content.is_empty() {
            (
                tr("ai-ask-placeholder").into(),
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
                &composer_mentions,
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
                    mention_quads: Vec::new(),
                    active_number_quads: Vec::new(),
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
        let mut mention_quads = Vec::new();
        for mention in &composer_mentions {
            mention_quads.extend(decorated_range_quads(
                &laid_out_lines,
                &mention.range,
                bounds,
                line_height,
                DecorationStyle {
                    background: rgba(with_alpha(theme.accent, 0x18)).into(),
                    border: rgba(with_alpha(theme.accent, 0x66)).into(),
                    border_width: px(1.0),
                    radius: px(4.0),
                },
            ));
        }
        let active_number_quads = active_page_number_edit
            .as_ref()
            .map(|active| {
                decorated_range_quads(
                    &laid_out_lines,
                    &active.digits,
                    bounds,
                    line_height,
                    DecorationStyle {
                        background: rgba(with_alpha(theme.accent, 0x38)).into(),
                        border: rgba(with_alpha(theme.accent, 0x99)).into(),
                        border_width: px(1.0),
                        radius: px(3.0),
                    },
                )
            })
            .unwrap_or_default();

        PrepaintState {
            lines: Some(laid_out_lines),
            cursor,
            cursor_position,
            selections,
            mention_quads,
            active_number_quads,
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

        for mention in prepaint.mention_quads.drain(..) {
            window.paint_quad(mention);
        }
        for active_number in prepaint.active_number_quads.drain(..) {
            window.paint_quad(active_number);
        }
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

fn input_text_runs(
    text: &str,
    base_run: TextRun,
    theme: Theme,
    composer_mentions: &[ComposerMention],
    marked_range: Option<&Range<usize>>,
) -> Vec<TextRun> {
    let mut boundaries = vec![0, text.len()];
    for mention in composer_mentions {
        boundaries.push(mention.range.start);
        boundaries.push(mention.range.end);
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

            let mention = composer_mentions
                .iter()
                .any(|mention| start >= mention.range.start && end <= mention.range.end);
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

fn decorated_range_quads(
    lines: &[LaidOutLine],
    range: &Range<usize>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    style: DecorationStyle,
) -> Vec<PaintQuad> {
    let fragments = selection_quads(lines, range, bounds, line_height, style.background);
    fragments
        .into_iter()
        .map(|fragment| {
            let fragment_bounds = fragment.bounds;
            quad(
                fragment_bounds,
                Corners::all(style.radius).clamp_radii_for_quad_size(fragment_bounds.size),
                style.background,
                Edges {
                    top: style.border_width,
                    right: style.border_width,
                    bottom: style.border_width,
                    left: style.border_width,
                },
                style.border,
                BorderStyle::Solid,
            )
        })
        .collect()
}

fn active_mention_range(text: &str, cursor: usize) -> Option<Range<usize>> {
    let cursor = cursor.min(text.len());
    let line_start = text[..cursor].rfind('\n').map_or(0, |ix| ix + 1);
    let line = &text[line_start..cursor];
    let at = line.rfind('@')?;
    let start = line_start + at;
    let valid_boundary = start == 0
        || text[..start]
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '{' | '"' | '\''));
    if !valid_boundary || text[start..cursor].contains('\n') {
        return None;
    }
    Some(start..cursor)
}

fn parse_composer_mentions(text: &str) -> Vec<ComposerMention> {
    find_mention_references(text)
        .into_iter()
        .map(|(range, reference)| ComposerMention { reference, range })
        .collect()
}

fn adjust_composer_mentions(
    mentions: &mut Vec<ComposerMention>,
    edited_range: &Range<usize>,
    inserted_len: usize,
) {
    let delta = inserted_len as isize - edited_range.len() as isize;
    mentions.retain_mut(|mention| {
        if edited_range.end <= mention.range.start {
            mention.range.start = (mention.range.start as isize + delta) as usize;
            mention.range.end = (mention.range.end as isize + delta) as usize;
            true
        } else {
            edited_range.start >= mention.range.end
        }
    });
}

fn adjust_mentions_for_page_number_edit(
    mentions: &mut [ComposerMention],
    mention_start: usize,
    new_digits: Range<usize>,
    delta: isize,
    validated_page: usize,
) {
    for mention in mentions {
        if mention.range.start == mention_start {
            mention.range.end = (mention.range.end as isize + delta) as usize;
            if validated_page > 0 {
                mention.reference.page = Some(validated_page);
            }
        } else if mention.range.start > mention_start {
            mention.range.start = (mention.range.start as isize + delta) as usize;
            mention.range.end = (mention.range.end as isize + delta) as usize;
        }
    }
    debug_assert!(new_digits.start <= new_digits.end);
}

fn validate_page_number(digits: &str, page_count: usize) -> usize {
    digits
        .parse::<usize>()
        .unwrap_or(1)
        .clamp(1, page_count.max(1))
}

fn request_prompt_without_mentions(text: &str, mentions: &[ComposerMention]) -> String {
    let mut result = String::with_capacity(text.len());
    let mut cursor = 0;
    for mention in mentions {
        if mention.range.start < cursor || mention.range.end > text.len() {
            continue;
        }
        result.push_str(&text[cursor..mention.range.start]);
        cursor = mention.range.end;
        let result_ends_with_space = result
            .chars()
            .next_back()
            .is_some_and(|ch| ch == ' ' || ch == '\t');
        if result_ends_with_space
            && text[cursor..]
                .chars()
                .next()
                .is_some_and(|ch| ch == ' ' || ch == '\t')
        {
            while text[cursor..]
                .chars()
                .next()
                .is_some_and(|ch| ch == ' ' || ch == '\t')
            {
                cursor += text[cursor..].chars().next().unwrap().len_utf8();
            }
        } else if result_ends_with_space
            && text[cursor..].chars().next().is_none_or(|ch| {
                ch == '\n' || matches!(ch, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}')
            })
        {
            while result
                .chars()
                .next_back()
                .is_some_and(|ch| ch == ' ' || ch == '\t')
            {
                result.pop();
            }
        }
    }
    result.push_str(&text[cursor..]);
    result.trim().to_string()
}

fn unresolved_page_references(messages: &[ChatMessage]) -> anyhow::Result<Vec<MentionReference>> {
    let image_count = messages
        .iter()
        .flat_map(ChatMessage::references)
        .filter(|reference| reference.kind == MentionKind::PageImage)
        .count();
    if image_count > MAX_PAGE_IMAGE_CONTEXTS {
        return Err(anyhow!(
            "too many page images attached: {image_count} requested, maximum is {MAX_PAGE_IMAGE_CONTEXTS}"
        ));
    }

    let mut seen = HashSet::new();
    let references = messages
        .iter()
        .flat_map(|message| {
            message.references().iter().copied().filter(|reference| {
                reference.kind.requires_page_number()
                    && !message.has_resolved_page_reference(*reference)
            })
        })
        .filter(|reference| seen.insert(*reference))
        .collect::<Vec<_>>();
    Ok(references)
}

fn validate_resolved_page_contexts(
    messages: &[ChatMessage],
    resolved: &[ResolvedPageContext],
) -> anyhow::Result<()> {
    let image_bytes = messages
        .iter()
        .map(ChatMessage::image_context_bytes)
        .sum::<usize>()
        + resolved
            .iter()
            .map(|context| match context {
                ResolvedPageContext::Image(image) => {
                    let reference = MentionReference::new(image.kind, image.page);
                    let occurrence_count = messages
                        .iter()
                        .filter(|message| {
                            message.references().contains(&reference)
                                && !message.has_resolved_page_reference(reference)
                        })
                        .count();
                    image.bytes.len() * occurrence_count
                }
                ResolvedPageContext::Text(_) => 0,
            })
            .sum::<usize>();
    if image_bytes > MAX_PAGE_IMAGE_BYTES {
        return Err(anyhow!(
            "attached page images are too large: {} MiB exceeds the {} MiB limit",
            image_bytes.div_ceil(1024 * 1024),
            MAX_PAGE_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    Ok(())
}

fn hydrate_page_contexts(messages: &mut [ChatMessage], resolved: &[ResolvedPageContext]) {
    for message in messages {
        let references = message.references().iter().copied().collect::<HashSet<_>>();
        for resolved in resolved {
            match resolved {
                ResolvedPageContext::Text(context) => {
                    let reference = MentionReference::new(context.kind, context.page);
                    if references.contains(&reference)
                        && !message.has_resolved_page_reference(reference)
                    {
                        message.attach_context(context.clone());
                    }
                }
                ResolvedPageContext::Image(image) => {
                    let reference = MentionReference::new(image.kind, image.page);
                    if references.contains(&reference)
                        && !message.has_resolved_page_reference(reference)
                    {
                        message.attach_image(image.clone());
                    }
                }
            }
        }
    }
}

fn ai_reference_to_library(reference: MentionReference) -> AiMentionReference {
    match reference.kind {
        MentionKind::Selection => AiMentionReference::Selection,
        MentionKind::DocumentText => AiMentionReference::DocumentText,
        MentionKind::Page => AiMentionReference::Page {
            page: reference.page.unwrap_or(1),
        },
        MentionKind::PageImage => AiMentionReference::PageImage {
            page: reference.page.unwrap_or(1),
        },
    }
}

fn library_reference_to_ai(reference: AiMentionReference) -> MentionReference {
    match reference {
        AiMentionReference::Selection => MentionReference::new(MentionKind::Selection, None),
        AiMentionReference::DocumentText => MentionReference::new(MentionKind::DocumentText, None),
        AiMentionReference::Page { page } => MentionReference::new(MentionKind::Page, Some(page)),
        AiMentionReference::PageImage { page } => {
            MentionReference::new(MentionKind::PageImage, Some(page))
        }
    }
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
    fn request_prompt_strips_structured_mentions_and_preserves_words() {
        let text = "Compare @page(12) with @selection\nusing @document text.";
        let mentions = parse_composer_mentions(text);
        assert_eq!(
            request_prompt_without_mentions(text, &mentions),
            "Compare with\nusing."
        );
    }

    #[test]
    fn edit_adjusts_ranges_and_removes_overlapping_mentions() {
        let mut mentions = parse_composer_mentions("@selection then @page(3)");
        adjust_composer_mentions(&mut mentions, &(0..0), 4);
        assert_eq!(mentions[0].range, 4..14);
        assert_eq!(mentions[1].range, 20..28);

        adjust_composer_mentions(&mut mentions, &(6..8), 1);
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].range, 19..27);
    }

    #[test]
    fn parser_restores_only_canonical_mentions_at_valid_boundaries() {
        let mentions =
            parse_composer_mentions("mail@example.com @page(2) (@page-image(4)) @selection");
        assert_eq!(
            mentions
                .into_iter()
                .map(|mention| mention.reference)
                .collect::<Vec<_>>(),
            vec![
                MentionReference::new(MentionKind::Page, Some(2)),
                MentionReference::new(MentionKind::PageImage, Some(4)),
                MentionReference::new(MentionKind::Selection, None),
            ]
        );
    }

    #[test]
    fn page_validation_defaults_and_clamps() {
        assert_eq!(validate_page_number("", 20), 1);
        assert_eq!(validate_page_number("0", 20), 1);
        assert_eq!(validate_page_number("12", 20), 12);
        assert_eq!(validate_page_number("99", 20), 20);
    }

    #[test]
    fn mention_trigger_requires_a_boundary() {
        assert_eq!(active_mention_range("ask @pa", 7), Some(4..7));
        assert_eq!(active_mention_range("mail@example", 12), None);
        assert_eq!(active_mention_range("(@page", 6), Some(1..6));
    }

    #[test]
    fn page_resolution_skips_hydrated_references_and_limits_images() {
        let reference = MentionReference::new(MentionKind::PageImage, Some(2));
        let mut hydrated = ChatMessage::user(
            "@page-image(2)".to_string(),
            String::new(),
            Vec::new(),
            vec![reference],
        );
        hydrated.attach_image(glyphst_ai::ai_chat::ImageContext::new(
            MentionKind::PageImage,
            Some(2),
            "image/jpeg",
            vec![1, 2, 3],
        ));
        assert!(unresolved_page_references(&[hydrated]).unwrap().is_empty());

        let messages = (1..=MAX_PAGE_IMAGE_CONTEXTS + 1)
            .map(|page| {
                let reference = MentionReference::new(MentionKind::PageImage, Some(page));
                ChatMessage::user(
                    reference.display_text(),
                    String::new(),
                    Vec::new(),
                    vec![reference],
                )
            })
            .collect::<Vec<_>>();
        assert!(unresolved_page_references(&messages).is_err());
    }

    #[test]
    fn hydration_preserves_existing_page_contexts() {
        let first = MentionReference::new(MentionKind::Page, Some(1));
        let second = MentionReference::new(MentionKind::Page, Some(2));
        let mut message = ChatMessage::user(
            "@page(1) @page(2)".to_string(),
            String::new(),
            Vec::new(),
            vec![first, second],
        );
        message.attach_context(ContextBlock::new(
            MentionKind::Page,
            Some(1),
            "First".to_string(),
        ));

        hydrate_page_contexts(
            std::slice::from_mut(&mut message),
            &[ResolvedPageContext::Text(ContextBlock::new(
                MentionKind::Page,
                Some(2),
                "Second".to_string(),
            ))],
        );

        assert!(message.has_resolved_page_reference(first));
        assert!(message.has_resolved_page_reference(second));
        let request = message.request_text();
        assert!(request.contains("First"));
        assert!(request.contains("Second"));
    }
}
