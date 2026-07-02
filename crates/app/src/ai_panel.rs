use std::{ops::Range, path::PathBuf, thread};

use crate::{
    ai_chat::{
        AiStreamEvent, ChatMessage, ContextBlock, DEFAULT_SESSION_TITLE, DocumentContext,
        MentionKind, Role, TITLE_GENERATION_PROMPT, find_mention_ranges, mention_matches_query,
        mentioned_kinds, session_title_for_prompt, stream_completion, thinking_level_description,
        title_from_response,
    },
    input,
    library::{AiSession, AiSessionMessage, AiSessionRole, LibraryStore, current_timestamp},
    markdown::render_markdown,
    pdf_view::PdfView,
    scrollbar::WithScrollbar as _,
    settings::{AiModel, AiProvider, AiThinkingLevel, ModelRef, SettingsStore, ui_rems},
    settings_window::PanelEvent,
};
use anyhow::anyhow;
use futures::{StreamExt, channel::mpsc};
use gpui::{
    Anchor, App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    FontWeight, GlobalElementId, InspectorElementId, IntoElement, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point as GpuiPoint, Render,
    ScrollHandle, SharedString, Style, Task, TextRun, TextStyle, UTF16Selection, WeakEntity, Window,
    WrappedLine, anchored, deferred, div, fill, point, prelude::*, px, relative, rgb, rgba, size,
};

gpui::actions!(ai_panel, [AiPanelClose, AiPanelSubmit]);

pub const AI_PANEL_WIDTH: f32 = 360.0;
const COMPLETION_MENU_WIDTH: f32 = 248.0;
const COMPLETION_MENU_ROW_HEIGHT: f32 = 46.0;
const DRAG_SELECT_THRESHOLD: f32 = 2.0;

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
    attachment_menu_open: bool,
    attachment_menu_anchor: Option<GpuiPoint<Pixels>>,
    model_menu_open: bool,
    model_menu_anchor: Option<GpuiPoint<Pixels>>,
    model_search_query: SharedString,
    model_menu_scroll: ScrollHandle,
    thinking_menu_open: bool,
    thinking_menu_anchor: Option<GpuiPoint<Pixels>>,
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

struct AiPanelInit {
    previous_focus_handle: FocusHandle,
    pdf_view: WeakEntity<PdfView>,
    document_path: PathBuf,
    current_page: usize,
    width: f32,
    mode: AiPanelMode,
    session_id: u64,
    sessions: Vec<AiSession>,
}

impl AiPanel {
    pub fn new(
        previous_focus_handle: FocusHandle,
        pdf_view: WeakEntity<PdfView>,
        document_path: PathBuf,
        current_page: usize,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Self {
        let session_id = cx.global_mut::<LibraryStore>().next_ai_session_id();
        let sessions = cx.global::<LibraryStore>().ai_sessions_for(&document_path);
        Self::new_with_mode(
            AiPanelInit {
                previous_focus_handle,
                pdf_view,
                document_path,
                current_page,
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
        pdf_view: WeakEntity<PdfView>,
        document_path: PathBuf,
        current_page: usize,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_mode(
            AiPanelInit {
                previous_focus_handle,
                pdf_view,
                document_path,
                current_page,
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
            pdf_view: init.pdf_view,
            document_path: init.document_path,
            current_page: init.current_page,
            session_id: init.session_id,
            session_title: DEFAULT_SESSION_TITLE.to_string(),
            session_created: 0,
            session_updated: 0,
            sessions: init.sessions,
            view: AiPanelView::Chat,
            mention_completion: None,
            last_cursor_pos: None,
            attachment_menu_open: false,
            attachment_menu_anchor: None,
            model_menu_open: false,
            model_menu_anchor: None,
            model_search_query: SharedString::default(),
            model_menu_scroll: ScrollHandle::new(),
            thinking_menu_open: false,
            thinking_menu_anchor: None,
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
        cx.emit(PanelEvent::Dismissed);
    }

    fn close(&mut self, _: &AiPanelClose, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(window, cx);
    }

    fn submit(&mut self, _: &AiPanelSubmit, _: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_mention_completion(cx) {
            return;
        }
        self.send_message(cx);
    }

    fn newline(&mut self, _: &input::Newline, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == AiPanelMode::Sidebar && self.confirm_mention_completion(cx) {
            return;
        }
        self.replace_text_in_range(None, "\n", window, cx);
    }

    fn backspace(&mut self, _: &input::Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.model_menu_open {
            let mut query = self.model_search_query.to_string();
            query.pop();
            self.model_search_query = query.into();
            cx.notify();
            return;
        }
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &input::Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.model_menu_open {
            self.model_search_query = SharedString::default();
            cx.notify();
            return;
        }
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn left(&mut self, _: &input::Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &input::Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn up(&mut self, _: &input::Up, window: &mut Window, cx: &mut Context<Self>) {
        if self.select_previous_mention_completion(cx) {
            return;
        }
        self.move_vertically(-1, false, window, cx);
    }

    fn down(&mut self, _: &input::Down, window: &mut Window, cx: &mut Context<Self>) {
        if self.select_next_mention_completion(cx) {
            return;
        }
        self.move_vertically(1, false, window, cx);
    }

    fn home(&mut self, _: &input::Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_start_offset(), cx);
    }

    fn end(&mut self, _: &input::End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_end_offset(), cx);
    }

    fn select_all(&mut self, _: &input::SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn select_left(&mut self, _: &input::SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &input::SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_up(&mut self, _: &input::SelectUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(-1, true, window, cx);
    }

    fn select_down(&mut self, _: &input::SelectDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(1, true, window, cx);
    }

    fn select_home(&mut self, _: &input::SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_start_offset(), cx);
    }

    fn select_end(&mut self, _: &input::SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_end_offset(), cx);
    }

    fn copy(&mut self, _: &input::Copy, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_selected_text(cx);
    }

    fn cut(&mut self, _: &input::Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.copy_selected_text(cx) {
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn paste(&mut self, _: &input::Paste, window: &mut Window, cx: &mut Context<Self>) {
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

        let settings = cx.global::<SettingsStore>().settings.ai.resolved_main();
        self.update_current_page(cx);
        let contexts = self.context_blocks_for_prompt(&prompt, cx);
        let document = self.document_context(cx);
        if self.mode == AiPanelMode::QuickAsk {
            self.messages.clear();
        } else if self.messages.is_empty() {
            self.session_title = session_title_for_prompt(&prompt);
        }
        if self.mode == AiPanelMode::Sidebar {
            self.content = SharedString::default();
            self.selected_range = 0..0;
        } else {
            self.content = prompt.clone().into();
            self.selected_range = prompt.len()..prompt.len();
        }
        self.selection_reversed = false;
        self.marked_range = None;
        self.mention_completion = None;
        self.last_cursor_pos = None;
        self.close_composer_menus();
        self.messages.push(ChatMessage::user(prompt, contexts));
        self.messages.push(ChatMessage::assistant(String::new()));
        self.loading = true;
        if self.mode == AiPanelMode::Sidebar {
            self.save_current_session(cx);
        }
        cx.notify();

        let request_messages = self.messages[..self.messages.len().saturating_sub(1)].to_vec();
        let (tx, mut rx) = mpsc::unbounded::<AiStreamEvent>();

        thread::spawn(move || {
            let mut send_event = |event| tx.unbounded_send(event).map_err(|_| anyhow!("closed"));
            let result = stream_completion(settings, request_messages, document, &mut send_event);
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
                    message.push_text(&delta);
                }
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
                self.pending_stream = None;
                if self.mode == AiPanelMode::Sidebar {
                    self.save_current_session(cx);
                }
            }
            AiStreamEvent::Done => {
                self.loading = false;
                self.pending_stream = None;
                if self.mode == AiPanelMode::Sidebar {
                    self.maybe_generate_title(cx);
                    self.save_current_session(cx);
                }
            }
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
        let mut title_messages = self.messages.clone();
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

    fn context_blocks_for_prompt(&self, prompt: &str, cx: &mut Context<Self>) -> Vec<ContextBlock> {
        mentioned_kinds(prompt)
            .into_iter()
            .map(|mention| self.resolve_mention(mention, cx))
            .collect()
    }

    fn resolve_mention(&self, mention: MentionKind, cx: &mut Context<Self>) -> ContextBlock {
        let resolved = self
            .pdf_view
            .update(cx, |pdf_view, _| match mention {
                MentionKind::CurrentPageText => {
                    let page = pdf_view.current_page();
                    let text = pdf_view
                        .current_page_text()
                        .unwrap_or_else(|| "No text is available on the current page.".to_string());
                    ContextBlock::new(mention, Some(page), text)
                }
                MentionKind::Selection => ContextBlock::new(
                    mention,
                    None,
                    pdf_view
                        .selected_text()
                        .unwrap_or_else(|| "No selected text is currently available.".to_string()),
                ),
                MentionKind::DocumentText => ContextBlock::new(
                    mention,
                    None,
                    pdf_view.document_text().unwrap_or_else(|| {
                        "No extracted text is available for this PDF.".to_string()
                    }),
                ),
            })
            .ok();

        resolved.unwrap_or_else(|| {
            let message = match mention {
                MentionKind::CurrentPageText => "No current page text is available.",
                MentionKind::Selection => "No selected text is currently available.",
                MentionKind::DocumentText => "No extracted text is available for this PDF.",
            };
            ContextBlock::new(mention, None, message.to_string())
        })
    }

    fn document_context(&self, cx: &mut Context<Self>) -> DocumentContext {
        let mut document = DocumentContext {
            file_name: self
                .document_path
                .file_name()
                .and_then(|file_name| file_name.to_str())
                .map(str::to_string),
            page_count: 0,
            current_page: self.current_page,
            has_selection: false,
        };

        if let Ok((page_count, current_page, has_selection)) =
            self.pdf_view.update(cx, |pdf_view, _| {
                (
                    pdf_view.page_count(),
                    pdf_view.current_page(),
                    pdf_view.selected_text().is_some(),
                )
            })
        {
            document.page_count = page_count;
            document.current_page = current_page;
            document.has_selection = has_selection;
        }

        document
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
        self.content = SharedString::default();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.mention_completion = None;
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
            })
            .collect();
        self.content = SharedString::default();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.mention_completion = None;
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
        if mentioned_kinds(&self.content).contains(&mention) {
            cx.notify();
            return;
        }

        let cursor = self.cursor_offset().min(self.content.len());
        let needs_leading_space = cursor > 0
            && self.content[..cursor]
                .chars()
                .last()
                .is_some_and(|ch| !ch.is_whitespace());
        let needs_trailing_space = self.content[cursor..]
            .chars()
            .next()
            .is_some_and(|ch| !ch.is_whitespace());
        let mut text = String::new();
        if needs_leading_space {
            text.push(' ');
        }
        text.push_str(mention.token());
        if needs_trailing_space || cursor == self.content.len() {
            text.push(' ');
        }
        self.replace_text_in_range(None, &text, window, cx);
    }

    fn remove_attachment(&mut self, mention: MentionKind, cx: &mut Context<Self>) {
        let mut content = self.content.to_string();
        for (range, kind) in find_mention_ranges(&content).into_iter().rev() {
            if kind != mention {
                continue;
            }

            let mut end = range.end;
            while end < content.len() {
                let Some(ch) = content[end..].chars().next() else {
                    break;
                };
                if ch == '\n' || !ch.is_whitespace() {
                    break;
                }
                end += ch.len_utf8();
            }
            content.replace_range(range.start..end, "");
        }

        self.content = content.into();
        let cursor = self.cursor_offset().min(self.content.len());
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        self.update_mention_completion();
        cx.notify();
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
        self.close_composer_menus();
        self.drag_origin = Some(event.position);

        let offset = self.index_for_mouse_position(event.position, window);
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
            (store.resolved_theme, store.settings.ai.quick_ask, base_text_style)
        };
        let loading = self.loading;
        let attached_mentions = mentioned_kinds(&self.content);
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
                div()
                    .flex()
                    .flex_col()
                    .when(!attached_mentions.is_empty(), |this| {
                        this.child(Self::render_attachment_chips(attached_mentions, theme, cx))
                    })
                    .child(
                        div()
                            .relative()
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
                                    this.child(render_markdown(
                                        &answer,
                                        theme,
                                        base_text_style.clone(),
                                    ))
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
        theme: crate::theme::Theme,
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
                                this.child(render_markdown(&content, theme, base_text_style))
                            }),
                    ),
            )
    }

    fn render_loading_indicator(theme: crate::theme::Theme) -> impl IntoElement {
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
        theme: crate::theme::Theme,
        scroll_handle: &ScrollHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut list = div().flex().flex_col().gap_1().p_2();
        list = list.child(
            div()
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
                    .text_size(ui_rems(13.0))
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
        theme: crate::theme::Theme,
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

    fn render_attachment_chips(
        mentions: Vec<MentionKind>,
        theme: crate::theme::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut row = div()
            .flex()
            .flex_wrap()
            .gap_1()
            .px_2()
            .pt_2()
            .text_size(ui_rems(11.0));

        for mention in mentions {
            row = row.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(rgb(theme.border_variant))
                    .bg(rgba(with_alpha(theme.accent, 0x14)))
                    .text_color(rgb(theme.text))
                    .child(div().text_color(rgb(theme.accent)).child(mention.label()))
                    .child(
                        div()
                            .text_color(rgb(theme.text_muted))
                            .cursor_pointer()
                            .hover(move |style| style.text_color(rgb(theme.text)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.remove_attachment(mention, cx);
                                    cx.stop_propagation();
                                }),
                            )
                            .child("×"),
                    ),
            );
        }

        row
    }

    fn render_attachment_menu(
        theme: crate::theme::Theme,
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

        for mention in MentionKind::ALL {
            menu = menu.child(
                div()
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
        theme: crate::theme::Theme,
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
                        "Type to search models…".into()
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
                            .child("Select provider/model"),
                    ),
            );

        if entries.is_empty() {
            menu = menu.child(
                div()
                    .px_2()
                    .py_2()
                    .text_color(rgb(theme.text_muted))
                    .child("No matching models"),
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
        theme: crate::theme::Theme,
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

        for level in AiThinkingLevel::supported_for(selected_provider, &selected_model) {
            let level = *level;
            let selected = level == selected_level;
            menu = menu.child(
                div()
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
        if self.model_menu_open {
            if !new_text.is_empty() {
                let mut query = self.model_search_query.to_string();
                query.push_str(new_text);
                self.model_search_query = query.into();
                cx.notify();
            }
            return;
        }

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
            (store.resolved_theme, store.settings.ai.clone(), base_text_style)
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
        let attached_mentions = mentioned_kinds(&self.content);
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
        for message in self.messages.clone() {
            messages = messages.child(Self::render_message(
                message,
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
                                    .child("AI"),
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
                        .when(!attached_mentions.is_empty(), |this| {
                            this.child(Self::render_attachment_chips(attached_mentions, theme, cx))
                        })
                        .child(
                            div()
                                .relative()
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
                                                .child("Attach"),
                                        )
                                        .child(
                                            div()
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
                                        .child(if loading { "Sending…" } else { "Send" }),
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
                            .child(Self::render_attachment_menu(theme, cx)),
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
