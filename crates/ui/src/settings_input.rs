//! Shared single-line text input used by settings controls and keymap filtering.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent, MouseUpEvent, Pixels,
    Point as GpuiPoint, Render, SharedString, UTF16Selection, Window, div, point, prelude::*, px,
    rgb,
};

use crate::{
    input,
    settings::SettingsStore,
    text_input::{
        MASK_GLYPH, SingleLineInput, SingleLineTextElement, index_for_mouse_position_with_scroll,
    },
};

pub enum SettingsInputEvent {
    Changed(SharedString),
}

pub struct SettingsTextInput {
    pub focus_handle: FocusHandle,
    pub content: SharedString,
    pub placeholder: SharedString,
    pub mask: bool,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<gpui::ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    scroll_offset: Pixels,
    drag_origin: Option<GpuiPoint<Pixels>>,
}

impl SettingsTextInput {
    pub fn new(
        content: SharedString,
        placeholder: SharedString,
        mask: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let len = content.len();
        Self {
            focus_handle: cx.focus_handle(),
            content,
            placeholder,
            mask,
            selected_range: len..len,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            scroll_offset: px(0.0),
            drag_origin: None,
        }
    }

    pub fn set_content(&mut self, content: SharedString, cx: &mut Context<Self>) {
        let len = content.len();
        self.content = content;
        self.selected_range = len..len;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
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

    fn home(&mut self, _: &input::Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &input::End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &input::Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &input::Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
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

    fn select_home(&mut self, _: &input::SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_end(&mut self, _: &input::SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), cx);
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
            let text = text.replace(['\n', '\r'], " ");
            self.replace_text_in_range(None, &text, window, cx);
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
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx);
        }
        cx.stop_propagation();
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.drag_origin = None;
        cx.stop_propagation();
    }

    fn on_mouse_down_out(
        &mut self,
        _: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.blur();
        self.cancel_selection(cx);
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
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: GpuiPoint<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        let display_index =
            index_for_mouse_position_with_scroll(line, *bounds, position, self.scroll_offset);
        self.content_offset_for_display_offset(display_index)
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = offset.min(self.content.len());
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify();
    }

    fn copy_selected_text(&self, cx: &mut Context<Self>) -> bool {
        if self.selected_range.is_empty() || self.mask {
            return false;
        }

        cx.write_to_clipboard(ClipboardItem::new_string(
            self.content[self.selected_range.clone()].to_string(),
        ));
        true
    }

    fn display_offset(&self, offset: usize) -> usize {
        if !self.mask {
            return offset.min(self.content.len());
        }

        self.content
            .char_indices()
            .take_while(|(ix, _)| *ix < offset)
            .count()
            * MASK_GLYPH.len()
    }

    fn display_range(&self, range: &Range<usize>) -> Range<usize> {
        self.display_offset(range.start)..self.display_offset(range.end)
    }

    fn content_offset_for_display_offset(&self, offset: usize) -> usize {
        if !self.mask {
            return offset.min(self.content.len());
        }

        let char_ix = (offset / MASK_GLYPH.len()).min(self.content.chars().count());
        self.content
            .char_indices()
            .nth(char_ix)
            .map(|(offset, _)| offset)
            .unwrap_or_else(|| self.content.len())
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
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
}

impl EntityInputHandler for SettingsTextInput {
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
            .map(|r| self.range_from_utf16(r))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range = None;
        cx.emit(SettingsInputEvent::Changed(self.content.clone()));
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
            .map(|r| self.range_from_utf16(r))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone());
        let selected_start = range.start;

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.marked_range =
            (!new_text.is_empty()).then_some(range.start..range.start + new_text.len());
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|r| {
                let start = offset_from_utf16_in_text(new_text, r.start);
                let end = offset_from_utf16_in_text(new_text, r.end);
                selected_start + start..selected_start + end
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;
        cx.emit(SettingsInputEvent::Changed(self.content.clone()));
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let display_range = self.display_range(&range);
        Some(Bounds::from_corners(
            point(
                bounds.left() + line.x_for_index(display_range.start) - self.scroll_offset,
                bounds.top(),
            ),
            point(
                bounds.left() + line.x_for_index(display_range.end) - self.scroll_offset,
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        pt: GpuiPoint<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let line = self.last_layout.as_ref()?;
        let _local = bounds.localize(&pt)?;
        let display_index = line.index_for_x(pt.x - bounds.left() + self.scroll_offset)?;
        let content_index = self.content_offset_for_display_offset(display_index);
        Some(self.offset_to_utf16(content_index))
    }
}

impl EventEmitter<SettingsInputEvent> for SettingsTextInput {}

impl Focusable for SettingsTextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SingleLineInput for SettingsTextInput {
    fn content(&self) -> &SharedString {
        &self.content
    }

    fn placeholder(&self) -> SharedString {
        self.placeholder.clone()
    }

    fn mask(&self) -> bool {
        self.mask
    }

    fn selected_range(&self) -> &Range<usize> {
        &self.selected_range
    }

    fn marked_range(&self) -> Option<&Range<usize>> {
        self.marked_range.as_ref()
    }

    fn cursor_offset(&self) -> usize {
        Self::cursor_offset(self)
    }

    fn scroll_offset(&self) -> Pixels {
        self.scroll_offset
    }

    fn display_offset(&self, offset: usize) -> usize {
        Self::display_offset(self, offset)
    }

    fn display_range(&self, range: &Range<usize>) -> Range<usize> {
        Self::display_range(self, range)
    }

    fn set_render_state(
        &mut self,
        layout: gpui::ShapedLine,
        bounds: Bounds<Pixels>,
        scroll: Pixels,
    ) {
        self.last_layout = Some(layout);
        self.last_bounds = Some(bounds);
        self.scroll_offset = scroll;
    }

    fn set_scroll_offset(&mut self, scroll: Pixels) {
        self.scroll_offset = scroll;
    }

    fn is_dragging(&self) -> bool {
        self.drag_origin.is_some()
    }

    fn content_offset_for_display_offset(&self, offset: usize) -> usize {
        Self::content_offset_for_display_offset(self, offset)
    }

    fn extend_selection_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.select_to(offset, cx);
    }

    fn end_drag(&mut self) {
        self.drag_origin = None;
    }
}

impl Render for SettingsTextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let theme = cx.global::<SettingsStore>().resolved_theme;
        let focused = self.focus_handle.is_focused(window);

        div()
            .key_context("SettingsInput TextInput")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_1()
            .min_w_0()
            .h(px(28.0))
            .px_2()
            .items_center()
            .border_1()
            .border_color(if focused {
                rgb(theme.accent)
            } else {
                rgb(theme.border_variant)
            })
            .bg(rgb(theme.surface_background))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_down_out(cx.listener(Self::on_mouse_down_out))
            .child(SingleLineTextElement::new(entity))
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
