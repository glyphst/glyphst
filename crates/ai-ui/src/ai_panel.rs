use std::{
    cell::Cell, collections::HashSet, io::Cursor, ops::Range, path::PathBuf, rc::Rc, sync::Arc,
    thread, time::Duration,
};

use crate::{
    AiContext, AiContextProvider, AiPageState, ResolvedPageContext, ResolvedRegionContext,
};
use anyhow::{Result, anyhow};
use futures::{StreamExt, channel::mpsc};
use glyphst_actions::ai_panel::AiPanelClose;
use glyphst_ai::ai_chat::{
    AiStreamEvent, ChatMessage, ContextBlock, DEFAULT_SESSION_TITLE, DocumentContext,
    MAX_PAGE_IMAGE_BYTES, MentionKind, MentionOccurrence, MentionReference, Role,
    TITLE_GENERATION_PROMPT, find_mention_references, session_title_for_prompt, stream_completion,
    thinking_level_description, title_from_response,
};
use glyphst_core::{RegionImageReference, SelectedPagesReference};
use glyphst_library::{
    AiMentionOccurrence, AiMentionReference, AiSession, AiSessionMessage, AiSessionRole,
    LibraryStore, current_timestamp,
};
use glyphst_settings::{
    AiApiStyle, AiModel, AiProvider, AiThinkingLevel, ModelRef, SettingsStore,
    localize::{tr, tr_with_number},
    ui_rems,
};
use glyphst_ui::{
    icons::{Icon, IconName, toolbar_icon_button},
    markdown::{Markdown, MarkdownContextMenuRequest, MarkdownElement, MarkdownStyle},
    picker::fuzzy_match,
    scrollbar::{ScrollbarHandle, WithScrollbar as _},
};
use gpui::{
    Anchor, Animation, AnimationExt, AnyElement, App, Bounds, ClipboardItem, Context, DismissEvent,
    ElementId, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, Image, ImageFormat,
    IntoElement, MouseButton, MouseDownEvent, ObjectFit, Pixels, Point as GpuiPoint, Render,
    ScrollDelta, ScrollHandle, ScrollWheelEvent, SharedString, Subscription, Task, TextStyle,
    WeakEntity, Window, anchored, canvas, deferred, div, img, point, prelude::*, px,
    transparent_black,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, InteractiveElementExt as _, Selectable as _, Sizable as _,
    ThemeColor as Theme, WindowExt as _,
    alert::Alert,
    button::{Button, ButtonGroup, ButtonVariants as _},
    input::{
        CompletionMenuOptions, CompletionProvider, CompletionTriggerEvent, Escape as InputEscape,
        Input, InputEvent, InputSpan, InputState, Rope, RopeExt,
    },
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    notification::Notification,
    popover::{Popover, PopoverState},
};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionResponse, CompletionTextEdit, Range as LspRange,
    TextEdit,
};

pub const AI_PANEL_WIDTH: f32 = 360.0;
const MENTION_PREVIEW_HIDE_DELAY: Duration = Duration::from_millis(150);
const MENTION_PREVIEW_MAX_CHARS: usize = 4_000;
const MENTION_PREVIEW_WIDTH: f32 = 320.0;
const MENTION_PREVIEW_TEXT_MAX_HEIGHT: f32 = 274.0;
const MENTION_PREVIEW_WINDOW_MARGIN: f32 = 8.0;
const MENTION_PREVIEW_BRIDGE_HEIGHT: f32 = 6.0;
const MENTION_COMPLETION_MIN_WIDTH: f32 = 280.0;
const MENTION_COMPLETION_MAX_WIDTH: f32 = 540.0;

struct AiValidationNotification;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiPanelEvent {
    Dismissed,
    Changed,
}

struct ModelMenuRenderState {
    selected_provider: AiProvider,
    selected_model: String,
    provider_models: Vec<(AiProvider, String, Vec<AiModel>)>,
    search_query: SharedString,
    search_input: Entity<InputState>,
    scroll_handle: ScrollHandle,
    panel: Entity<AiPanel>,
    popover: Entity<PopoverState>,
}

#[derive(Clone)]
enum ModelMenuEntry {
    Header(String),
    Model(AiProvider, AiModel),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MentionMatch {
    kind: MentionKind,
    highlights: Vec<Range<usize>>,
    score: usize,
    canonical_ix: usize,
}

#[derive(Clone, Debug, PartialEq)]
struct ComposerMention {
    reference: MentionReference,
    range: Range<usize>,
    captured: Option<CapturedMention>,
}

#[derive(Clone, Debug, PartialEq)]
enum CapturedMention {
    Text(String),
    Region(RegionImageReference),
    Pages(SelectedPagesReference),
}

impl ComposerMention {
    fn captured_text(&self) -> Option<&str> {
        match self.captured.as_ref()? {
            CapturedMention::Text(text) => Some(text),
            CapturedMention::Region(_) | CapturedMention::Pages(_) => None,
        }
    }

    fn captured_region(&self) -> Option<&RegionImageReference> {
        match self.captured.as_ref()? {
            CapturedMention::Region(region) => Some(region),
            CapturedMention::Text(_) | CapturedMention::Pages(_) => None,
        }
    }

    fn captured_pages(&self) -> Option<&SelectedPagesReference> {
        match self.captured.as_ref()? {
            CapturedMention::Pages(pages) => Some(pages),
            CapturedMention::Text(_) | CapturedMention::Region(_) => None,
        }
    }
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

#[derive(Clone)]
enum MentionPreviewContent {
    Text {
        markdown: Entity<Markdown>,
        truncated: bool,
    },
    Image(MentionPreviewImage),
}

enum MentionPreviewPayload {
    Text(String),
    Image(MentionPreviewImage),
}

#[derive(Clone)]
struct MentionPreviewImage {
    image: Arc<Image>,
    width: u32,
    height: u32,
}

#[derive(Clone)]
enum MentionPreviewStatus {
    Loading,
    Ready(MentionPreviewContent),
    Error(String),
}

#[derive(Clone)]
struct MentionPreview {
    reference: MentionReference,
    source_range: Range<usize>,
    trigger_bounds: Bounds<Pixels>,
    position: GpuiPoint<Pixels>,
    anchor: Anchor,
    generation: u64,
    status: MentionPreviewStatus,
}

#[derive(Clone)]
struct MentionPreviewRenderOptions {
    width: f32,
    image_height: f32,
    scroll_handle: ScrollHandle,
    bounds_cell: Rc<Cell<Option<Bounds<Pixels>>>>,
    theme: Theme,
    base_text_style: TextStyle,
}

#[derive(Clone)]
struct TrackedScrollHandle {
    handle: ScrollHandle,
    interacted: Rc<Cell<bool>>,
}

impl ScrollbarHandle for TrackedScrollHandle {
    fn set_offset(&self, point: GpuiPoint<Pixels>) {
        self.interacted.set(true);
        self.handle.set_offset(point);
    }

    fn offset(&self) -> GpuiPoint<Pixels> {
        self.handle.offset()
    }

    fn content_size(&self) -> gpui::Size<Pixels> {
        (self.handle.max_offset() + self.handle.bounds().size.into()).into()
    }

    fn start_drag(&self) {
        self.interacted.set(true);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GenerationTarget {
    id: u64,
    assistant_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GenerationFailure {
    assistant_index: usize,
    message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubmitAction {
    Edit,
    Retry(usize),
    Send,
}

fn submit_action(editing: bool, failure: Option<&GenerationFailure>) -> SubmitAction {
    if editing {
        SubmitAction::Edit
    } else if let Some(failure) = failure {
        SubmitAction::Retry(failure.assistant_index)
    } else {
        SubmitAction::Send
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ComposerSnapshot {
    content: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    mentions: Vec<ComposerMention>,
}

#[derive(Clone)]
struct EditingTurn {
    message_index: usize,
    saved_composer: ComposerSnapshot,
}

struct PreparedComposerMessage {
    display_prompt: String,
    submitted_mentions: Vec<ComposerMention>,
    user_message: ChatMessage,
    document: DocumentContext,
}

#[derive(Clone)]
struct MessageContextMenu {
    position: GpuiPoint<Pixels>,
    popup: Entity<PopupMenu>,
}

struct MessageContextMenuBuildState {
    message_index: usize,
    is_user: bool,
    can_mutate: bool,
    has_later_messages: bool,
    selected_text: Option<SharedString>,
    link_url: Option<SharedString>,
}

struct MessageRenderInput {
    index: usize,
    message: ChatMessage,
    markdown: Entity<Markdown>,
    theme: Theme,
    panel_width: f32,
    base_text_style: TextStyle,
    panel: Entity<AiPanel>,
    editing_focused: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CopyFeedbackTarget {
    Message(usize),
    Error,
}

struct MentionCompletionProvider {
    panel: WeakEntity<AiPanel>,
}

pub struct AiPanel {
    focus_handle: FocusHandle,
    composer_input: Entity<InputState>,
    edit_input: Entity<InputState>,
    model_search_input: Entity<InputState>,
    _composer_input_subscription: Subscription,
    _edit_input_subscription: Subscription,
    _model_search_input_subscription: Subscription,
    previous_focus_handle: FocusHandle,
    mode: AiPanelMode,
    messages: Vec<ChatMessage>,
    width: f32,
    content: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    loading: bool,
    scroll_handle: ScrollHandle,
    sessions_scroll: ScrollHandle,
    pending_stream: Option<Task<()>>,
    stream_render_task: Option<Task<()>>,
    generation_id: u64,
    active_generation: Option<GenerationTarget>,
    generation_failure: Option<GenerationFailure>,
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
    composer_mentions: Vec<ComposerMention>,
    model_search_query: SharedString,
    model_menu_scroll: ScrollHandle,
    markdown_views: Vec<Entity<Markdown>>,
    validation_notice: Option<String>,
    editing_turn: Option<EditingTurn>,
    copied_target: Option<CopyFeedbackTarget>,
    copy_feedback_generation: u64,
    copy_feedback_task: Option<Task<()>>,
    follow_output: bool,
    scroll_epoch: u64,
    message_scroll_interacted: Rc<Cell<bool>>,
    message_scroll_task: Option<Task<()>>,
    message_context_menu: Option<MessageContextMenu>,
    mention_preview: Option<MentionPreview>,
    mention_preview_generation: u64,
    pending_mention_preview: Option<Task<()>>,
    mention_preview_hide_task: Option<Task<()>>,
    mention_preview_scroll: ScrollHandle,
    mention_preview_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

fn loading_dot_opacity(delta: f32, phase: f32) -> f32 {
    let wave = (1.0 + (std::f32::consts::TAU * (delta - phase)).cos()) / 2.0;
    0.25 + 0.75 * wave
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

mod chat;
mod composer;
mod helpers;
mod mentions;
mod menus;
mod render;
mod rendering;
mod sessions;

impl AiPanel {
    pub fn new(
        previous_focus_handle: FocusHandle,
        context_provider: impl AiContextProvider,
        document_path: PathBuf,
        page_state: AiPageState,
        width: f32,
        window: &mut Window,
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
            window,
            cx,
        )
    }

    pub fn new_quick_ask(
        previous_focus_handle: FocusHandle,
        context_provider: impl AiContextProvider,
        document_path: PathBuf,
        page_state: AiPageState,
        width: f32,
        window: &mut Window,
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
            window,
            cx,
        )
    }

    fn new_with_mode(init: AiPanelInit, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (min_rows, max_rows) = match init.mode {
            AiPanelMode::Sidebar => (3, 8),
            AiPanelMode::QuickAsk => (1, 4),
        };
        let panel = cx.weak_entity();
        let composer_provider = Rc::new(MentionCompletionProvider {
            panel: panel.clone(),
        });
        let edit_provider = Rc::new(MentionCompletionProvider { panel });
        let composer_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx)
                .auto_grow(min_rows, max_rows)
                .soft_wrap(true)
                .submit_on_enter(true)
                .placeholder(tr("ai-ask-placeholder"));
            input.lsp.completion_provider = Some(composer_provider.clone());
            input
        });
        let edit_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx)
                .auto_grow(1, usize::MAX)
                .soft_wrap(true)
                .submit_on_enter(true);
            input.lsp.completion_provider = Some(edit_provider.clone());
            input
        });
        let model_search_input = cx
            .new(|cx| InputState::new(window, cx).placeholder(tr("ai-search-models-placeholder")));
        let focus_handle = composer_input.focus_handle(cx);
        let composer_input_subscription =
            cx.subscribe_in(&composer_input, window, Self::on_composer_input_event);
        let edit_input_subscription =
            cx.subscribe_in(&edit_input, window, Self::on_composer_input_event);
        let model_search_input_subscription = cx.subscribe_in(
            &model_search_input,
            window,
            Self::on_model_search_input_event,
        );

        Self {
            focus_handle,
            composer_input,
            edit_input,
            model_search_input,
            _composer_input_subscription: composer_input_subscription,
            _edit_input_subscription: edit_input_subscription,
            _model_search_input_subscription: model_search_input_subscription,
            previous_focus_handle: init.previous_focus_handle,
            mode: init.mode,
            messages: Vec::new(),
            width: init.width,
            content: SharedString::default(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            loading: false,
            scroll_handle: ScrollHandle::new(),
            sessions_scroll: ScrollHandle::new(),
            pending_stream: None,
            stream_render_task: None,
            generation_id: 0,
            active_generation: None,
            generation_failure: None,
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
            composer_mentions: Vec::new(),
            model_search_query: SharedString::default(),
            model_menu_scroll: ScrollHandle::new(),
            markdown_views: Vec::new(),
            validation_notice: None,
            editing_turn: None,
            copied_target: None,
            copy_feedback_generation: 0,
            copy_feedback_task: None,
            follow_output: true,
            scroll_epoch: 0,
            message_scroll_interacted: Rc::new(Cell::new(false)),
            message_scroll_task: None,
            message_context_menu: None,
            mention_preview: None,
            mention_preview_generation: 0,
            pending_mention_preview: None,
            mention_preview_hide_task: None,
            mention_preview_scroll: ScrollHandle::new(),
            mention_preview_bounds: Rc::new(Cell::new(None)),
        }
    }

    pub fn set_width(&mut self, width: f32) {
        if (self.width - width).abs() > f32::EPSILON {
            self.invalidate_mention_preview();
        }
        self.width = width;
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing_turn.is_some() {
            self.cancel_editing(window, cx);
        }
        let stopped_generation = self.stop_generation(cx);
        self.update_current_page(cx);
        self.invalidate_mention_preview();
        if self.mode == AiPanelMode::Sidebar && !stopped_generation {
            self.save_current_session(cx);
        }
        self.previous_focus_handle.focus(window, cx);
        cx.emit(AiPanelEvent::Dismissed);
    }

    fn close(&mut self, _: &AiPanelClose, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_message_context_menu(cx) {
            return;
        }
        let active_input = self.active_composer_input();
        let completion_closed = active_input.update(cx, |input, cx| {
            input.handle_action_for_context_menu(Box::new(InputEscape), window, cx)
        });
        if completion_closed {
            return;
        }
        if self.editing_turn.is_some() {
            self.cancel_editing(window, cx);
            return;
        }
        if self.stop_generation(cx) {
            return;
        }
        self.dismiss(window, cx);
    }
}

impl EventEmitter<AiPanelEvent> for AiPanel {}

impl Focusable for AiPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests;
