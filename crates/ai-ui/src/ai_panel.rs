use std::{
    cell::Cell, collections::HashSet, ops::Range, path::PathBuf, rc::Rc, sync::Arc, thread,
    time::Duration,
};

use crate::{
    AiContext, AiContextProvider, AiPageState, ResolvedPageContext, ResolvedRegionContext,
};
use anyhow::anyhow;
use futures::{StreamExt, channel::mpsc};
use glyphst_actions::ai_panel::{AiPanelClose, AiPanelSubmit};
use glyphst_ai::ai_chat::{
    AiStreamEvent, ChatMessage, ContextBlock, DEFAULT_SESSION_TITLE, DocumentContext,
    MAX_PAGE_IMAGE_BYTES, MentionKind, MentionOccurrence, MentionReference, Role,
    TITLE_GENERATION_PROMPT, find_mention_references, session_title_for_prompt, stream_completion,
    thinking_level_description, title_from_response,
};
use glyphst_core::RegionImageReference;
use glyphst_library::{
    AiMentionOccurrence, AiMentionReference, AiSession, AiSessionMessage, AiSessionRole,
    LibraryStore, current_timestamp,
};
use glyphst_settings::{
    AiModel, AiProvider, AiThinkingLevel, ModelRef, SettingsStore, localize::tr, theme::Theme,
    ui_rems,
};
use glyphst_ui::{
    icons::{Icon, IconName, toolbar_icon_button},
    markdown::{Markdown, MarkdownContextMenuRequest, MarkdownElement, MarkdownStyle},
    picker::fuzzy_match,
    scrollbar::{ScrollbarHandle, WithScrollbar as _},
    tooltip::text_tooltip,
};
use gpui::{
    Anchor, Animation, AnimationExt, AnyElement, App, ClipboardItem, Context, ElementId, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, HighlightStyle, IntoElement, MouseButton,
    MouseDownEvent, Pixels, Point as GpuiPoint, Render, ScrollDelta, ScrollHandle, ScrollStrategy,
    ScrollWheelEvent, SharedString, StyledText, Subscription, Task, TextStyle,
    UniformListScrollHandle, Window, anchored, deferred, div, point, prelude::*, px, rgb, rgba,
    uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};

pub const AI_PANEL_WIDTH: f32 = 360.0;
const COMPLETION_MENU_MIN_WIDTH: f32 = 280.0;
const COMPLETION_MENU_MAX_WIDTH: f32 = 540.0;
const COMPLETION_MENU_ROW_HEIGHT: f32 = 40.0;
const COMPLETION_MENU_VISIBLE_ROWS: usize = 8;
const VALIDATION_NOTICE_DURATION: Duration = Duration::from_secs(4);
const COPY_FEEDBACK_DURATION: Duration = Duration::from_millis(1500);

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
    search_input: Entity<InputState>,
    scroll_handle: ScrollHandle,
}

#[derive(Clone, Copy)]
struct CompletionMenuPlacement {
    position: GpuiPoint<Pixels>,
    anchor: Anchor,
    width: f32,
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
}

impl ComposerMention {
    fn captured_text(&self) -> Option<&str> {
        match self.captured.as_ref()? {
            CapturedMention::Text(text) => Some(text),
            CapturedMention::Region(_) => None,
        }
    }

    fn captured_region(&self) -> Option<&RegionImageReference> {
        match self.captured.as_ref()? {
            CapturedMention::Region(region) => Some(region),
            CapturedMention::Text(_) => None,
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
    message_index: usize,
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

pub struct AiPanel {
    focus_handle: FocusHandle,
    composer_input: Entity<InputState>,
    model_search_input: Entity<InputState>,
    _composer_input_subscription: Subscription,
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
    mention_completion: Option<MentionCompletion>,
    mention_completion_scroll: UniformListScrollHandle,
    composer_mentions: Vec<ComposerMention>,
    attachment_menu_open: bool,
    attachment_menu_anchor: Option<GpuiPoint<Pixels>>,
    model_menu_open: bool,
    model_menu_anchor: Option<GpuiPoint<Pixels>>,
    model_search_query: SharedString,
    model_menu_scroll: ScrollHandle,
    thinking_menu_open: bool,
    thinking_menu_anchor: Option<GpuiPoint<Pixels>>,
    markdown_views: Vec<Entity<Markdown>>,
    validation_notice: Option<String>,
    validation_toast: Option<String>,
    validation_notice_task: Option<Task<()>>,
    editing_turn: Option<EditingTurn>,
    edit_blur_subscription: Option<Subscription>,
    copied_target: Option<CopyFeedbackTarget>,
    copy_feedback_generation: u64,
    copy_feedback_task: Option<Task<()>>,
    follow_output: bool,
    scroll_epoch: u64,
    message_scroll_interacted: Rc<Cell<bool>>,
    message_scroll_task: Option<Task<()>>,
    message_context_menu: Option<MessageContextMenu>,
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
        let composer_input = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(min_rows, max_rows)
                .soft_wrap(true)
                .submit_on_enter(true)
                .placeholder(tr("ai-ask-placeholder"))
        });
        let model_search_input = cx
            .new(|cx| InputState::new(window, cx).placeholder(tr("ai-search-models-placeholder")));
        let focus_handle = composer_input.focus_handle(cx);
        let composer_input_subscription =
            cx.subscribe_in(&composer_input, window, Self::on_composer_input_event);
        let model_search_input_subscription = cx.subscribe_in(
            &model_search_input,
            window,
            Self::on_model_search_input_event,
        );

        Self {
            focus_handle,
            composer_input,
            model_search_input,
            _composer_input_subscription: composer_input_subscription,
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
            mention_completion: None,
            mention_completion_scroll: UniformListScrollHandle::new(),
            composer_mentions: Vec::new(),
            attachment_menu_open: false,
            attachment_menu_anchor: None,
            model_menu_open: false,
            model_menu_anchor: None,
            model_search_query: SharedString::default(),
            model_menu_scroll: ScrollHandle::new(),
            thinking_menu_open: false,
            thinking_menu_anchor: None,
            markdown_views: Vec::new(),
            validation_notice: None,
            validation_toast: None,
            validation_notice_task: None,
            editing_turn: None,
            edit_blur_subscription: None,
            copied_target: None,
            copy_feedback_generation: 0,
            copy_feedback_task: None,
            follow_output: true,
            scroll_epoch: 0,
            message_scroll_interacted: Rc::new(Cell::new(false)),
            message_scroll_task: None,
            message_context_menu: None,
        }
    }

    pub fn set_width(&mut self, width: f32) {
        self.width = width;
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing_turn.is_some() {
            self.cancel_editing(window, cx);
        }
        let stopped_generation = self.stop_generation(cx);
        self.update_current_page(cx);
        if self.mode == AiPanelMode::Sidebar && !stopped_generation {
            self.save_current_session(cx);
        }
        self.previous_focus_handle.focus(window, cx);
        cx.emit(AiPanelEvent::Dismissed);
    }

    fn close(&mut self, _: &AiPanelClose, window: &mut Window, cx: &mut Context<Self>) {
        if self.message_context_menu.take().is_some()
            || self.attachment_menu_open
            || self.model_menu_open
            || self.thinking_menu_open
            || self.mention_completion.is_some()
        {
            self.close_composer_menus();
            self.mention_completion = None;
            cx.notify();
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

    fn submit(&mut self, _: &AiPanelSubmit, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_mention_completion(cx) {
            return;
        }
        match submit_action(
            self.editing_turn.is_some(),
            self.generation_failure.as_ref(),
        ) {
            SubmitAction::Edit => self.submit_edited_message(window, cx),
            SubmitAction::Retry(assistant_index) => self.retry_assistant(assistant_index, cx),
            SubmitAction::Send => self.send_message(cx),
        }
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
