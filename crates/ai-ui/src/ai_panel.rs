use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    io::Cursor,
    ops::Range,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    thread,
    time::Duration,
};

use crate::{
    AiContext, AiContextProvider, AiPageState, ResolvedPageContext, ResolvedRegionContext,
};
use anyhow::anyhow;
use futures::{StreamExt, channel::mpsc};
use glyphst_actions::{
    ai_panel::{AiPanelClose, AiPanelSubmit},
    input,
};
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
    scrollbar::{ScrollableHandle, WithScrollbar as _},
    text_edit::{
        EditGroup, EditHistory, backward_word_deletion_start, forward_word_deletion_end, line_end,
        line_start, next_grapheme_boundary, next_word_end, offset_from_utf16, offset_to_utf16,
        previous_grapheme_boundary, previous_word_start, range_from_utf16, range_to_utf16,
    },
    tooltip::text_tooltip,
};
use gpui::{
    Anchor, Animation, AnimationExt, AnyElement, App, AvailableSpace, BorderStyle, Bounds,
    ClipboardItem, Context, Corners, CursorStyle, DispatchPhase, Edges, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    FontWeight, GlobalElementId, HighlightStyle, Hitbox, HitboxBehavior, Image, ImageFormat,
    InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ObjectFit, PaintQuad, Pixels, Point as GpuiPoint, Render, ScrollDelta,
    ScrollHandle, ScrollStrategy, ScrollWheelEvent, SharedString, Style, StyledText, Subscription,
    Task, TextRun, TextStyle, TransformationMatrix, UTF16Selection, UniformListScrollHandle,
    Window, WrappedLine, anchored, canvas, deferred, div, fill, img, point, prelude::*, px, quad,
    relative, rgb, rgba, size, uniform_list,
};

pub const AI_PANEL_WIDTH: f32 = 360.0;
const COMPLETION_MENU_MIN_WIDTH: f32 = 280.0;
const COMPLETION_MENU_MAX_WIDTH: f32 = 540.0;
const COMPLETION_MENU_ROW_HEIGHT: f32 = 40.0;
const COMPLETION_MENU_VISIBLE_ROWS: usize = 8;
const DRAG_SELECT_THRESHOLD: f32 = 2.0;
const MENTION_PREVIEW_HIDE_DELAY: Duration = Duration::from_millis(150);
const MENTION_PREVIEW_MAX_CHARS: usize = 4_000;
const MENTION_PREVIEW_WIDTH: f32 = 320.0;
const MENTION_PREVIEW_TEXT_MAX_HEIGHT: f32 = 240.0;
const MENTION_PREVIEW_WINDOW_MARGIN: f32 = 8.0;
const MENTION_PREVIEW_BRIDGE_HEIGHT: f32 = 6.0;
const MENTION_PREVIEW_HEADER_HEIGHT: f32 = 34.0;
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
    scroll_handle: ScrollHandle,
    focused: bool,
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct DisplayMention {
    kind: MentionKind,
    source_range: Range<usize>,
    range: Range<usize>,
    icon_range: Range<usize>,
    number_range: Option<Range<usize>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ComposerDisplay {
    text: String,
    source_to_display: Vec<usize>,
    display_to_source: Vec<usize>,
    mentions: Vec<DisplayMention>,
}

impl ComposerDisplay {
    fn source_offset(&self, display_offset: usize) -> usize {
        self.display_to_source
            .get(display_offset.min(self.display_to_source.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0)
    }

    fn display_offset(&self, source_offset: usize) -> usize {
        self.source_to_display
            .get(source_offset.min(self.source_to_display.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0)
    }

    fn display_range(&self, source_range: &Range<usize>) -> Range<usize> {
        self.display_offset(source_range.start)..self.display_offset(source_range.end)
    }
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
    source: MentionPreviewSource,
    reference: MentionReference,
    source_range: Range<usize>,
    trigger_bounds: Bounds<Pixels>,
    position: GpuiPoint<Pixels>,
    anchor: Anchor,
    generation: u64,
    status: MentionPreviewStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MentionPreviewSource {
    Composer,
    Message(usize),
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

impl ScrollableHandle for TrackedScrollHandle {
    fn max_offset(&self) -> GpuiPoint<Pixels> {
        self.handle.max_offset()
    }

    fn set_offset(&self, point: GpuiPoint<Pixels>) {
        self.interacted.set(true);
        self.handle.set_offset(point);
    }

    fn offset(&self) -> GpuiPoint<Pixels> {
        self.handle.offset()
    }

    fn viewport(&self) -> Bounds<Pixels> {
        self.handle.bounds()
    }

    fn drag_started(&self) {
        self.interacted.set(true);
    }
}

#[derive(Clone)]
struct MentionHitbox {
    source_range: Range<usize>,
    bounds: Bounds<Pixels>,
}

#[derive(Clone)]
struct MessageMentionHitbox {
    message_index: usize,
    hitbox: MentionHitbox,
    mention: ComposerMention,
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
    active_page_number_edit: Option<ActivePageNumberEdit>,
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
    edit_focus_handle: FocusHandle,
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
    active_page_number_edit: Option<ActivePageNumberEdit>,
    last_display: Option<ComposerDisplay>,
    last_cursor_pos: Option<GpuiPoint<Pixels>>,
    mention_hitboxes: Vec<MentionHitbox>,
    message_mention_hitboxes: Vec<MessageMentionHitbox>,
    mention_preview: Option<MentionPreview>,
    mention_preview_generation: u64,
    pending_mention_preview: Option<Task<()>>,
    mention_preview_hide_task: Option<Task<()>>,
    mention_preview_scroll: ScrollHandle,
    mention_preview_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    attachment_menu_open: bool,
    attachment_menu_anchor: Option<GpuiPoint<Pixels>>,
    model_menu_open: bool,
    model_menu_anchor: Option<GpuiPoint<Pixels>>,
    model_search_query: SharedString,
    model_search_history: EditHistory<SharedString>,
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
    composer_history: EditHistory<ComposerSnapshot>,
}

struct LaidOutLine {
    start: usize,
    line: WrappedLine,
}

struct AiInputElement {
    panel: Entity<AiPanel>,
    source: AiInputSource,
}

struct AiInputLayoutState {
    display: ComposerDisplay,
    measurement: Rc<RefCell<Option<AiInputMeasurement>>>,
}

struct AiInputMeasurement {
    width: Pixels,
    lines: Vec<WrappedLine>,
}

#[derive(Clone)]
enum AiInputSource {
    Composer,
    EditedMessage,
    UserMessage {
        message_index: usize,
        content: SharedString,
        mentions: Vec<ComposerMention>,
    },
}

fn input_line_constraints(
    mode: AiPanelMode,
    source: &AiInputSource,
    messages_empty: bool,
) -> (bool, usize, Option<usize>) {
    match source {
        AiInputSource::Composer => {
            let empty_chat = mode == AiPanelMode::Sidebar && messages_empty;
            if mode == AiPanelMode::QuickAsk {
                (empty_chat, 1, Some(4))
            } else {
                (empty_chat, 3, Some(8))
            }
        }
        AiInputSource::EditedMessage | AiInputSource::UserMessage { .. } => (false, 1, None),
    }
}

fn constrained_input_height(
    measured_height: Pixels,
    line_height: Pixels,
    min_lines: usize,
    max_lines: Option<usize>,
) -> Pixels {
    let min_height = line_height * min_lines as f32;
    let height = measured_height.max(min_height);
    max_lines.map_or(height, |max_lines| {
        height.min(line_height * max_lines as f32)
    })
}

fn loading_dot_opacity(delta: f32, phase: f32) -> f32 {
    let wave = (1.0 + (std::f32::consts::TAU * (delta - phase)).cos()) / 2.0;
    0.25 + 0.75 * wave
}

struct PrepaintState {
    lines: Option<Vec<LaidOutLine>>,
    display: ComposerDisplay,
    cursor: Option<PaintQuad>,
    cursor_position: Option<GpuiPoint<Pixels>>,
    selections: Vec<PaintQuad>,
    mention_quads: Vec<PaintQuad>,
    active_number_quads: Vec<PaintQuad>,
    mention_icons: Vec<(Bounds<Pixels>, MentionKind)>,
    mention_hitboxes: Vec<MentionHitbox>,
    hitbox: Option<Hitbox>,
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

mod chat;
mod composer;
mod helpers;
mod input_element;
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
            edit_focus_handle: cx.focus_handle(),
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
            active_page_number_edit: None,
            last_display: None,
            last_cursor_pos: None,
            mention_hitboxes: Vec::new(),
            message_mention_hitboxes: Vec::new(),
            mention_preview: None,
            mention_preview_generation: 0,
            pending_mention_preview: None,
            mention_preview_hide_task: None,
            mention_preview_scroll: ScrollHandle::new(),
            mention_preview_bounds: Rc::new(Cell::new(None)),
            attachment_menu_open: false,
            attachment_menu_anchor: None,
            model_menu_open: false,
            model_menu_anchor: None,
            model_search_query: SharedString::default(),
            model_search_history: EditHistory::default(),
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
            composer_history: EditHistory::default(),
        }
    }

    pub fn set_width(&mut self, width: f32) {
        if (self.width - width).abs() > f32::EPSILON {
            self.last_lines.clear();
            self.last_bounds = None;
            self.last_display = None;
            self.last_cursor_pos = None;
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
        if self.finish_page_number_edit(cx) {
            return;
        }
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
