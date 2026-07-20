use std::ops::Range;

use crate::{
    input,
    library::HighlightColor,
    settings::{SettingsStore, ui_rems},
};
pub use glyphst_actions::note_editor::{NoteCancel, NoteSave};
use glyphst_ui::text_edit::{
    EditGroup, EditHistory, TextSnapshot, backward_word_deletion_start, forward_word_deletion_end,
    line_end, line_start, next_grapheme_boundary, next_word_end, offset_from_utf16,
    offset_to_utf16, previous_grapheme_boundary, previous_word_start, range_from_utf16,
    range_to_utf16,
};
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, Pixels, Point as GpuiPoint, Render, SharedString, Style, TextRun,
    UTF16Selection, Window, WrappedLine, div, fill, point, prelude::*, px, relative, rgb, rgba,
    size,
};

const DRAG_SELECT_THRESHOLD: f32 = 2.0;

pub enum NoteEditorEvent {
    Save {
        body: SharedString,
        color: HighlightColor,
    },
    Cancel,
}

pub struct NoteEditor {
    focus_handle: FocusHandle,
    previous_focus_handle: FocusHandle,
    content: SharedString,
    color: HighlightColor,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_lines: Vec<LaidOutLine>,
    last_bounds: Option<Bounds<Pixels>>,
    drag_origin: Option<GpuiPoint<Pixels>>,
    history: EditHistory<TextSnapshot>,
}

struct LaidOutLine {
    start: usize,
    line: WrappedLine,
}

struct NoteTextElement {
    editor: Entity<NoteEditor>,
}

struct PrepaintState {
    lines: Option<Vec<LaidOutLine>>,
    cursor: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
}

impl NoteEditor {
    pub fn new(
        previous_focus_handle: FocusHandle,
        content: SharedString,
        color: HighlightColor,
        cx: &mut Context<Self>,
    ) -> Self {
        let content_len = content.len();
        Self {
            focus_handle: cx.focus_handle(),
            previous_focus_handle,
            content,
            color,
            selected_range: 0..content_len,
            selection_reversed: false,
            marked_range: None,
            last_lines: Vec::new(),
            last_bounds: None,
            drag_origin: None,
            history: EditHistory::default(),
        }
    }

    fn save(&mut self, _: &NoteSave, window: &mut Window, cx: &mut Context<Self>) {
        self.previous_focus_handle.focus(window, cx);
        cx.emit(NoteEditorEvent::Save {
            body: self.content.clone(),
            color: self.color,
        });
    }

    fn cancel(&mut self, _: &NoteCancel, window: &mut Window, cx: &mut Context<Self>) {
        self.previous_focus_handle.focus(window, cx);
        cx.emit(NoteEditorEvent::Cancel);
    }

    fn newline(&mut self, _: &input::Newline, _: &mut Window, cx: &mut Context<Self>) {
        self.replace_selected_text("\n", EditGroup::Isolated, cx);
    }

    fn backspace(&mut self, _: &input::Backspace, _: &mut Window, cx: &mut Context<Self>) {
        let (range, group) = if self.selected_range.is_empty() {
            (
                previous_grapheme_boundary(&self.content, self.cursor_offset())
                    ..self.cursor_offset(),
                EditGroup::Backspace,
            )
        } else {
            (self.selected_range.clone(), EditGroup::Isolated)
        };
        self.replace_range(range, "", group, cx);
    }

    fn delete(&mut self, _: &input::Delete, _: &mut Window, cx: &mut Context<Self>) {
        let (range, group) = if self.selected_range.is_empty() {
            (
                self.cursor_offset()..next_grapheme_boundary(&self.content, self.cursor_offset()),
                EditGroup::Delete,
            )
        } else {
            (self.selected_range.clone(), EditGroup::Isolated)
        };
        self.replace_range(range, "", group, cx);
    }

    fn delete_word_backward(
        &mut self,
        _: &input::DeleteWordBackward,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = if self.selected_range.is_empty() {
            note_word_deletion_start(&self.content, self.cursor_offset())..self.cursor_offset()
        } else {
            self.selected_range.clone()
        };
        self.replace_range(range, "", EditGroup::Isolated, cx);
    }

    fn delete_word_forward(
        &mut self,
        _: &input::DeleteWordForward,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = if self.selected_range.is_empty() {
            self.cursor_offset()..note_word_deletion_end(&self.content, self.cursor_offset())
        } else {
            self.selected_range.clone()
        };
        self.replace_range(range, "", EditGroup::Isolated, cx);
    }

    fn delete_to_line_start(
        &mut self,
        _: &input::DeleteToLineStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = if self.selected_range.is_empty() {
            line_start(&self.content, self.cursor_offset())..self.cursor_offset()
        } else {
            self.selected_range.clone()
        };
        self.replace_range(range, "", EditGroup::Isolated, cx);
    }

    fn delete_to_line_end(
        &mut self,
        _: &input::DeleteToLineEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = if self.selected_range.is_empty() {
            self.cursor_offset()..line_end(&self.content, self.cursor_offset())
        } else {
            self.selected_range.clone()
        };
        self.replace_range(range, "", EditGroup::Isolated, cx);
    }

    fn left(&mut self, _: &input::Left, _: &mut Window, cx: &mut Context<Self>) {
        self.history.break_group();
        if self.selected_range.is_empty() {
            self.move_to(
                previous_grapheme_boundary(&self.content, self.cursor_offset()),
                cx,
            );
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &input::Right, _: &mut Window, cx: &mut Context<Self>) {
        self.history.break_group();
        if self.selected_range.is_empty() {
            self.move_to(
                next_grapheme_boundary(&self.content, self.selected_range.end),
                cx,
            );
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn word_left(&mut self, _: &input::WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.history.break_group();
        if self.selected_range.is_empty() {
            self.move_to(previous_word_start(&self.content, self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn word_right(&mut self, _: &input::WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.history.break_group();
        if self.selected_range.is_empty() {
            self.move_to(next_word_end(&self.content, self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn up(&mut self, _: &input::Up, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(-1, false, window, cx);
    }

    fn down(&mut self, _: &input::Down, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(1, false, window, cx);
    }

    fn home(&mut self, _: &input::Home, _: &mut Window, cx: &mut Context<Self>) {
        self.history.break_group();
        self.move_to(line_start(&self.content, self.cursor_offset()), cx);
    }

    fn end(&mut self, _: &input::End, _: &mut Window, cx: &mut Context<Self>) {
        self.history.break_group();
        self.move_to(line_end(&self.content, self.cursor_offset()), cx);
    }

    fn document_start(&mut self, _: &input::DocumentStart, _: &mut Window, cx: &mut Context<Self>) {
        self.history.break_group();
        self.move_to(0, cx);
    }

    fn document_end(&mut self, _: &input::DocumentEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.history.break_group();
        self.move_to(self.content.len(), cx);
    }

    fn select_all(&mut self, _: &input::SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.history.break_group();
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn select_left(&mut self, _: &input::SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.history.break_group();
        self.select_to(
            previous_grapheme_boundary(&self.content, self.cursor_offset()),
            cx,
        );
    }

    fn select_right(&mut self, _: &input::SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.history.break_group();
        self.select_to(
            next_grapheme_boundary(&self.content, self.cursor_offset()),
            cx,
        );
    }

    fn select_word_left(
        &mut self,
        _: &input::SelectWordLeft,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.history.break_group();
        self.select_to(previous_word_start(&self.content, self.cursor_offset()), cx);
    }

    fn select_word_right(
        &mut self,
        _: &input::SelectWordRight,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.history.break_group();
        self.select_to(next_word_end(&self.content, self.cursor_offset()), cx);
    }

    fn select_up(&mut self, _: &input::SelectUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(-1, true, window, cx);
    }

    fn select_down(&mut self, _: &input::SelectDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(1, true, window, cx);
    }

    fn select_home(&mut self, _: &input::SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.history.break_group();
        self.select_to(line_start(&self.content, self.cursor_offset()), cx);
    }

    fn select_end(&mut self, _: &input::SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.history.break_group();
        self.select_to(line_end(&self.content, self.cursor_offset()), cx);
    }

    fn select_document_start(
        &mut self,
        _: &input::SelectDocumentStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.history.break_group();
        self.select_to(0, cx);
    }

    fn select_document_end(
        &mut self,
        _: &input::SelectDocumentEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.history.break_group();
        self.select_to(self.content.len(), cx);
    }

    fn copy(&mut self, _: &input::Copy, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_selected_text(cx);
    }

    fn cut(&mut self, _: &input::Cut, _: &mut Window, cx: &mut Context<Self>) {
        if self.copy_selected_text(cx) {
            self.replace_selected_text("", EditGroup::Isolated, cx);
        }
    }

    fn paste(&mut self, _: &input::Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            self.replace_selected_text(&text, EditGroup::Isolated, cx);
        }
    }

    fn undo(&mut self, _: &input::Undo, _: &mut Window, cx: &mut Context<Self>) {
        let current = self.snapshot();
        if let Some(snapshot) = self.history.undo(current) {
            self.restore_snapshot(snapshot, cx);
        }
    }

    fn redo(&mut self, _: &input::Redo, _: &mut Window, cx: &mut Context<Self>) {
        let current = self.snapshot();
        if let Some(snapshot) = self.history.redo(current) {
            self.restore_snapshot(snapshot, cx);
        }
    }

    fn set_color(&mut self, color: HighlightColor, cx: &mut Context<Self>) {
        if self.color != color {
            self.color = color;
            cx.notify();
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        self.drag_origin = Some(event.position);
        self.history.break_group();

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
            self.history.break_group();
            self.select_to(self.index_for_mouse_position(event.position, window), cx);
            cx.stop_propagation();
        }
    }

    fn cancel_selection(&mut self, cx: &mut Context<Self>) {
        self.drag_origin = None;
        if !self.selected_range.is_empty() {
            self.history.break_group();
            self.move_to(self.cursor_offset(), cx);
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

    fn move_vertically(
        &mut self,
        direction: isize,
        extend_selection: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.history.break_group();
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

    fn copy_selected_text(&self, cx: &mut Context<Self>) -> bool {
        if self.selected_range.is_empty() {
            return false;
        }

        cx.write_to_clipboard(ClipboardItem::new_string(
            self.content[self.selected_range.clone()].to_string(),
        ));
        true
    }

    fn snapshot(&self) -> TextSnapshot {
        TextSnapshot::new(
            self.content.clone(),
            self.selected_range.clone(),
            self.selection_reversed,
            self.marked_range.clone(),
        )
    }

    fn restore_snapshot(&mut self, snapshot: TextSnapshot, cx: &mut Context<Self>) {
        self.content = snapshot.content;
        self.selected_range = snapshot.selected_range;
        self.selection_reversed = snapshot.selection_reversed;
        self.marked_range = snapshot.marked_range;
        cx.notify();
    }

    fn replace_selected_text(&mut self, new_text: &str, group: EditGroup, cx: &mut Context<Self>) {
        let range = self
            .marked_range
            .clone()
            .unwrap_or_else(|| self.selected_range.clone());
        self.replace_range(range, new_text, group, cx);
    }

    fn replace_range(
        &mut self,
        range: Range<usize>,
        new_text: &str,
        group: EditGroup,
        cx: &mut Context<Self>,
    ) {
        let before = self.snapshot();
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        let cursor = range.start + new_text.len();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        let after = self.snapshot();
        self.history.record(before, &after, group);
        cx.notify();
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
}

impl EntityInputHandler for NoteEditor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = range_from_utf16(&self.content, &range_utf16);
        actual_range.replace(range_to_utf16(&self.content, &range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: range_to_utf16(&self.content, &self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| range_to_utf16(&self.content, range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
        self.history.break_group();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let group = if self.marked_range.is_some() {
            EditGroup::Composition
        } else {
            EditGroup::Typing
        };
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| range_from_utf16(&self.content, range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.replace_range(range, new_text, group, cx);
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
            .map(|range_utf16| range_from_utf16(&self.content, range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let selected_range_start = range.start;
        let before = self.snapshot();

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.marked_range =
            (!new_text.is_empty()).then_some(range.start..range.start + new_text.len());
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| {
                let start = offset_from_utf16(new_text, range_utf16.start);
                let end = offset_from_utf16(new_text, range_utf16.end);
                selected_range_start + start..selected_range_start + end
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;
        let after = self.snapshot();
        self.history.record(before, &after, EditGroup::Composition);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = range_from_utf16(&self.content, &range_utf16);
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
        Some(offset_to_utf16(
            &self.content,
            self.index_for_mouse_position(point, window),
        ))
    }
}

impl EventEmitter<NoteEditorEvent> for NoteEditor {}

impl Focusable for NoteEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NoteEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let theme = cx.global::<SettingsStore>().resolved_theme;
        let selected_color = self.color;
        let mut color_palette = div().flex().flex_row().gap_2();
        for color in HighlightColor::ALL {
            color_palette = color_palette.child(
                div()
                    .id(format!("note-editor-color-{color:?}"))
                    .w(px(22.0))
                    .h(px(22.0))
                    .border_1()
                    .border_color(if selected_color == color {
                        rgb(theme.accent)
                    } else {
                        rgb(theme.border)
                    })
                    .bg(rgba(theme.note_color(color)))
                    .cursor_pointer()
                    .hover(move |style| style.border_color(rgb(theme.accent)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.set_color(color, cx);
                            cx.stop_propagation();
                        }),
                    ),
            );
        }

        div()
            .key_context("NoteEditor TextInput")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .w(px(560.0))
            .border_1()
            .border_color(rgb(theme.border))
            .overflow_hidden()
            .bg(rgb(theme.surface_background))
            .text_color(rgb(theme.text))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_backward))
            .on_action(cx.listener(Self::delete_word_forward))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::delete_to_line_end))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::document_start))
            .on_action(cx.listener(Self::document_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::select_document_start))
            .on_action(cx.listener(Self::select_document_end))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(42.0))
                    .px_3()
                    .border_b(px(1.0))
                    .border_color(rgb(theme.border))
                    .text_size(ui_rems(14.0))
                    .child(crate::fl!("note-title")),
            )
            .child(
                div()
                    .h(px(190.0))
                    .p_3()
                    .cursor(CursorStyle::IBeam)
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_mouse_down_out(cx.listener(Self::on_mouse_down_out))
                    .on_mouse_move(cx.listener(Self::on_mouse_move))
                    .child(NoteTextElement { editor: entity }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(36.0))
                    .px_3()
                    .border_t(px(1.0))
                    .border_color(rgb(theme.border))
                    .text_size(ui_rems(12.0))
                    .text_color(rgb(theme.text_muted))
                    .child(color_palette)
                    .child(crate::fl!("note-editor-help")),
            )
    }
}

impl IntoElement for NoteTextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for NoteTextElement {
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
        let input = self.editor.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();
        let theme = cx.global::<SettingsStore>().resolved_theme;
        let (display_text, text_color) = if content.is_empty() {
            ("Write a note…".into(), rgba(theme.placeholder_text).into())
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
                eprintln!("failed to shape note input: {error:#}");
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
        let focus_handle = self.editor.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.editor.clone()),
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
                eprintln!("failed to paint note input: {error:#}");
            }
            y += laid_out.line.size(line_height).height;
        }

        if let Some(cursor) = prepaint.cursor.take() {
            window.paint_quad(cursor);
        }

        self.editor.update(cx, |input, _| {
            input.last_lines = lines;
            input.last_bounds = Some(bounds);
        });
    }
}

fn note_word_deletion_start(text: &str, offset: usize) -> usize {
    backward_word_deletion_start(text, offset).max(line_start(text, offset))
}

fn note_word_deletion_end(text: &str, offset: usize) -> usize {
    forward_word_deletion_end(text, offset).min(line_end(text, offset))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_deletion_does_not_cross_logical_lines() {
        let text = "first\nsecond";
        let second_start = "first\n".len();

        assert_eq!(note_word_deletion_start(text, second_start), second_start);
        assert_eq!(note_word_deletion_end(text, "first".len()), "first".len());
    }
}
