use std::{io::Read as _, ops::Range, thread};

use crate::{
    settings::{AiProvider, AiSettings, SettingsStore},
    settings_panel::PanelEvent,
};
use anyhow::{Context as _, Result, anyhow};
use futures::{StreamExt, channel::mpsc};
use gpui::{
    App, Bounds, Context, CursorStyle, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    Pixels, Point as GpuiPoint, Render, ScrollHandle, SharedString, Style, Task, TextRun,
    UTF16Selection, Window, WrappedLine, div, fill, point, prelude::*, px, relative, rgb, rgba,
    size,
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
        AiPanelPaste
    ]
);

const AI_PANEL_WIDTH: f32 = 320.0;
const MAX_CONTEXT_CHARS: usize = 12_000;

#[derive(Clone, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
}

#[derive(Clone)]
struct ChatMessage {
    role: Role,
    content: String,
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
    pdf_selection: Option<String>,
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
    selections: Vec<PaintQuad>,
}

impl AiPanel {
    pub fn new(
        previous_focus_handle: FocusHandle,
        pdf_selection: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
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
            pdf_selection: pdf_selection.and_then(|selection| {
                let selection = selection.trim();
                (!selection.is_empty()).then(|| selection.chars().take(MAX_CONTEXT_CHARS).collect())
            }),
        }
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        self.move_vertically(-1, window, cx);
    }

    fn down(&mut self, _: &AiPanelDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(1, window, cx);
    }

    fn home(&mut self, _: &AiPanelHome, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let start = self.content[..cursor]
            .rfind('\n')
            .map(|ix| ix + '\n'.len_utf8())
            .unwrap_or(0);
        self.move_to(start, cx);
    }

    fn end(&mut self, _: &AiPanelEnd, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let end = self.content[cursor..]
            .find('\n')
            .map(|ix| cursor + ix)
            .unwrap_or(self.content.len());
        self.move_to(end, cx);
    }

    fn select_all(&mut self, _: &AiPanelSelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
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
        self.content = SharedString::default();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.messages.push(ChatMessage {
            role: Role::User,
            content: prompt,
        });
        self.messages.push(ChatMessage {
            role: Role::Assistant,
            content: String::new(),
        });
        self.loading = true;
        cx.notify();

        let request_messages = self.messages[..self.messages.len().saturating_sub(1)].to_vec();
        let pdf_selection = self.pdf_selection.clone();
        let (tx, mut rx) = mpsc::unbounded::<AiStreamEvent>();

        thread::spawn(move || {
            let mut send_event = |event| tx.unbounded_send(event).map_err(|_| anyhow!("closed"));
            let result =
                stream_completion(settings, pdf_selection, request_messages, &mut send_event);
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
                        this.apply_stream_event(event);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn apply_stream_event(&mut self, event: AiStreamEvent) {
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
            }
            AiStreamEvent::Done => {
                self.loading = false;
                self.pending_stream = None;
            }
        }
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

        cx.notify();
    }

    fn move_vertically(&mut self, direction: isize, window: &mut Window, cx: &mut Context<Self>) {
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
        self.move_to(self.index_for_mouse_position(target, window), cx);
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
        let background = if is_user {
            rgba(with_alpha(theme.accent, 0x22))
        } else {
            rgba(theme.control_background)
        };
        let border = if is_user {
            rgba(with_alpha(theme.accent, 0x55))
        } else {
            rgba(theme.control_border)
        };
        let content = if message.content.is_empty() {
            "…".to_string()
        } else {
            message.content
        };

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
                    .max_w(px(260.0))
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .bg(background)
                    .px_2()
                    .py_1()
                    .text_size(px(13.0))
                    .text_color(rgb(theme.text))
                    .child(render_text_lines(&content)),
            )
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

        let mut messages = div().flex().flex_col().gap_3().p_3();
        if self.messages.is_empty() {
            messages = messages.child(
                div()
                    .text_size(px(13.0))
                    .text_color(rgb(theme.text_muted))
                    .child("Ask questions about the PDF. Selected text is included as context."),
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
            .on_action(cx.listener(Self::paste))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(38.0))
                    .px_3()
                    .border_b(px(1.0))
                    .border_color(rgb(theme.panel_border))
                    .text_size(px(14.0))
                    .child("AI Assistant")
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .text_size(px(12.0))
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
                            .child("Close"),
                    ),
            )
            .child(
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
                            .h(px(92.0))
                            .rounded_sm()
                            .border_1()
                            .border_color(rgb(theme.control_border))
                            .bg(rgb(theme.control_background))
                            .p_2()
                            .cursor(CursorStyle::IBeam)
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
                            .on_mouse_move(cx.listener(Self::on_mouse_move))
                            .child(AiInputElement { panel: entity }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .text_size(px(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child("Ctrl+Enter sends · Shift+Enter newline")
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
                                    .child(if self.loading { "Sending…" } else { "Send" }),
                            ),
                    ),
            )
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
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
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
            if let Some(marked_range) = input.marked_range.as_ref() {
                vec![
                    TextRun {
                        len: marked_range.start,
                        ..base_run.clone()
                    },
                    TextRun {
                        len: marked_range.end - marked_range.start,
                        underline: Some(gpui::UnderlineStyle {
                            color: Some(base_run.color),
                            thickness: px(1.0),
                            wavy: false,
                        }),
                        ..base_run.clone()
                    },
                    TextRun {
                        len: display_text.len() - marked_range.end,
                        ..base_run
                    },
                ]
                .into_iter()
                .filter(|run| run.len > 0)
                .collect()
            } else {
                vec![base_run]
            }
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
        });
    }
}

fn stream_completion(
    settings: AiSettings,
    pdf_selection: Option<String>,
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
        AiProvider::OpenAi => stream_openai(&client, settings, pdf_selection, messages, send_event),
        AiProvider::Anthropic => {
            stream_anthropic(&client, settings, pdf_selection, messages, send_event)
        }
    }
}

fn stream_openai(
    client: &reqwest::blocking::Client,
    settings: AiSettings,
    pdf_selection: Option<String>,
    messages: Vec<ChatMessage>,
    send_event: &mut impl FnMut(AiStreamEvent) -> Result<()>,
) -> Result<()> {
    let url = openai_chat_url(settings.base_url.as_deref());
    let mut request_messages = Vec::new();
    if let Some(system) = system_prompt(pdf_selection) {
        request_messages.push(json!({
            "role": "system",
            "content": system,
        }));
    }
    for message in messages {
        request_messages.push(json!({
            "role": match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            },
            "content": message.content,
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
    pdf_selection: Option<String>,
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
                "content": message.content,
            })
        })
        .collect::<Vec<_>>();

    let mut body = json!({
        "model": settings.model,
        "messages": request_messages,
        "max_tokens": 4096,
        "stream": true,
    });
    if let Some(system) = system_prompt(pdf_selection) {
        body["system"] = json!(system);
    }

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

fn system_prompt(pdf_selection: Option<String>) -> Option<String> {
    pdf_selection.map(|selection| {
        format!(
            "You are an AI assistant embedded in a PDF viewer. Help the user understand and work with the document. Relevant selected text:\n\n{selection}"
        )
    })
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
