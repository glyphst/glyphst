use std::ops::Range;

use crate::input;
use crate::menu;
use crate::scrollbar::uniform_list_scrollbar_for;
use crate::settings::SettingsStore;
use crate::style::{INPUT_ROW_HEIGHT, RADIUS_LG, RADIUS_MD, TextSize, modal_shadow};
use crate::text_input::SingleLineTextElement;
use crate::theme::Theme;
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, CursorStyle, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent, MouseUpEvent, Pixels,
    Point as GpuiPoint, Render, ScrollStrategy, ShapedLine, SharedString, UTF16Selection,
    UniformListScrollHandle, Window, div, point, prelude::*, px, rgb, rgba, uniform_list,
};

const LIST_VERTICAL_PADDING: f32 = 4.0;

pub enum PickerEvent {
    Dismissed,
}

pub trait PickerDelegate: Sized + 'static {
    fn key_context(&self) -> &'static str;
    fn placeholder(&self) -> SharedString;
    fn empty_message(&self) -> SharedString;
    fn width(&self) -> f32;
    fn row_height(&self) -> f32;
    fn max_visible_matches(&self) -> usize;
    fn match_count(&self) -> usize;
    fn update_matches(&mut self, query: &str);
    fn render_match(
        &self,
        match_ix: usize,
        theme: Theme,
        previous_focus_handle: &FocusHandle,
        window: &Window,
    ) -> AnyElement;
    fn confirm(
        &mut self,
        match_ix: usize,
        previous_focus_handle: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    );
    fn confirm_input(
        &mut self,
        _: SharedString,
        _: &FocusHandle,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) {
    }
    fn remove_match(&mut self, _: usize, _: &mut Context<Picker<Self>>) {}
    fn selection_changed(&mut self, _match_ix: usize, _cx: &mut Context<Picker<Self>>) {}
    fn dismissed(&mut self, _cx: &mut Context<Picker<Self>>) {}
}

pub struct Picker<D: PickerDelegate> {
    focus_handle: FocusHandle,
    previous_focus_handle: FocusHandle,
    content: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    scroll_offset: Pixels,
    drag_origin: Option<GpuiPoint<Pixels>>,
    selected_ix: usize,
    list_scroll_handle: UniformListScrollHandle,
    delegate: D,
}

impl<D: PickerDelegate> Picker<D> {
    pub fn new(previous_focus_handle: FocusHandle, delegate: D, cx: &mut Context<Self>) -> Self {
        Self::with_content_and_selected_ix(previous_focus_handle, delegate, "".into(), 0, cx)
    }

    pub fn new_with_selected_ix(
        previous_focus_handle: FocusHandle,
        delegate: D,
        selected_ix: usize,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_content_and_selected_ix(
            previous_focus_handle,
            delegate,
            "".into(),
            selected_ix,
            cx,
        )
    }

    pub fn with_content(
        previous_focus_handle: FocusHandle,
        delegate: D,
        content: SharedString,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_content_and_selected_ix(previous_focus_handle, delegate, content, 0, cx)
    }

    fn with_content_and_selected_ix(
        previous_focus_handle: FocusHandle,
        delegate: D,
        content: SharedString,
        selected_ix: usize,
        cx: &mut Context<Self>,
    ) -> Self {
        let content_len = content.len();
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            previous_focus_handle,
            content,
            selected_range: 0..content_len,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            scroll_offset: px(0.0),
            drag_origin: None,
            selected_ix,
            list_scroll_handle: UniformListScrollHandle::new(),
            delegate,
        };
        this.update_matches();
        this
    }

    fn update_matches(&mut self) {
        self.delegate.update_matches(self.content.as_ref());
        self.selected_ix = self
            .selected_ix
            .min(self.delegate.match_count().saturating_sub(1));
        self.center_selected();
    }

    fn match_count(&self) -> usize {
        self.delegate.match_count()
    }

    fn row_height(&self) -> f32 {
        self.delegate.row_height()
    }

    fn list_viewport_height(&self) -> f32 {
        self.row_height() * self.match_count().min(self.delegate.max_visible_matches()) as f32
    }

    fn center_selected(&mut self) {
        if self.match_count() == 0 {
            return;
        }

        self.list_scroll_handle
            .scroll_to_item_strict(self.selected_ix, ScrollStrategy::Center);
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.delegate.dismissed(cx);
        self.previous_focus_handle.focus(window, cx);
        cx.emit(PickerEvent::Dismissed);
    }

    fn confirm_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_ix >= self.match_count() {
            let content = self.content.clone();
            self.previous_focus_handle.focus(window, cx);
            self.delegate
                .confirm_input(content, &self.previous_focus_handle, window, cx);
            self.dismiss(window, cx);
            return;
        }

        self.previous_focus_handle.focus(window, cx);
        self.delegate
            .confirm(self.selected_ix, &self.previous_focus_handle, window, cx);
        self.delegate.dismissed(cx);
        cx.emit(PickerEvent::Dismissed);
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_selected(window, cx);
    }

    fn cancel(&mut self, _: &menu::Cancel, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(window, cx);
    }

    fn select_next(&mut self, _: &menu::SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_ix + 1 < self.match_count() {
            self.selected_ix += 1;
            self.center_selected();
            self.delegate.selection_changed(self.selected_ix, cx);
            cx.notify();
        }
    }

    fn select_prev(&mut self, _: &menu::SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_ix > 0 {
            self.selected_ix -= 1;
            self.center_selected();
            self.delegate.selection_changed(self.selected_ix, cx);
            cx.notify();
        }
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

    fn paste(&mut self, _: &input::Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = text.replace(['\n', '\r'], " ");
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn remove_selected(&mut self, _: &menu::Remove, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_ix < self.match_count() {
            self.delegate.remove_match(self.selected_ix, cx);
            self.update_matches();
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

    fn on_mouse_down_out(&mut self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
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

        crate::text_input::index_for_mouse_position_with_scroll(
            line,
            *bounds,
            position,
            self.scroll_offset,
        )
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

    fn copy_selected_text(&self, cx: &mut Context<Self>) -> bool {
        if self.selected_range.is_empty() {
            return false;
        }

        cx.write_to_clipboard(ClipboardItem::new_string(
            self.content[self.selected_range.clone()].to_string(),
        ));
        true
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

impl<D: PickerDelegate> EntityInputHandler for Picker<D> {
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
        self.update_matches();
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
        self.update_matches();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let last_layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(range.start) - self.scroll_offset,
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(range.end) - self.scroll_offset,
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: GpuiPoint<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let line = self.last_layout.as_ref()?;
        let utf8_index = line.index_for_x(point.x - bounds.left() + self.scroll_offset)?;
        Some(self.offset_to_utf16(utf8_index))
    }
}

impl<D: PickerDelegate> EventEmitter<PickerEvent> for Picker<D> {}

impl<D: PickerDelegate> Focusable for Picker<D> {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl<D: PickerDelegate> crate::text_input::SingleLineInput for Picker<D> {
    fn content(&self) -> &SharedString {
        &self.content
    }

    fn placeholder(&self) -> SharedString {
        self.delegate.placeholder()
    }

    fn mask(&self) -> bool {
        false
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

    fn set_render_state(&mut self, layout: ShapedLine, bounds: Bounds<Pixels>, scroll: Pixels) {
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

    fn extend_selection_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.select_to(offset, cx);
    }

    fn end_drag(&mut self) {
        self.drag_origin = None;
    }
}

impl<D: PickerDelegate> Render for Picker<D> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let theme = cx.global::<SettingsStore>().resolved_theme;
        let match_count = self.match_count();
        let list_height = self.list_viewport_height();
        let scroll_handle = self.list_scroll_handle.clone();

        div()
            .key_context(self.delegate.key_context())
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .w(px(self.delegate.width()))
            .rounded(px(RADIUS_LG))
            .border_1()
            .border_color(rgb(theme.border_variant))
            .shadow(modal_shadow(theme))
            .overflow_hidden()
            .bg(rgb(theme.surface_background))
            .text_size(TextSize::Default.rems())
            .text_color(rgb(theme.text))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::remove_selected))
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(INPUT_ROW_HEIGHT))
                    .px_3()
                    .border_b(px(1.0))
                    .border_color(rgb(theme.border_variant))
                    .cursor(CursorStyle::IBeam)
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_mouse_down_out(cx.listener(Self::on_mouse_down_out))
                    .child(SingleLineTextElement::new(entity)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .py(px(LIST_VERTICAL_PADDING))
                    .when(match_count == 0, |this| {
                        this.child(
                            div()
                                .h(px(44.0))
                                .flex()
                                .items_center()
                                .px_3()
                                .text_color(rgb(theme.text_muted))
                                .child(self.delegate.empty_message()),
                        )
                    })
                    .when(match_count > 0, |this| {
                        this.child(
                            div()
                                .h(px(list_height))
                                .overflow_hidden()
                                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                                .child(
                                    uniform_list(
                                        "picker-matches",
                                        match_count,
                                        cx.processor(
                                            move |this, visible_range: Range<usize>, window, cx| {
                                                let theme =
                                                    cx.global::<SettingsStore>().resolved_theme;
                                                let row_height = this.row_height();
                                                let previous_focus_handle =
                                                    this.previous_focus_handle.clone();

                                                visible_range
                                                    .map(|match_ix| {
                                                        let selected = match_ix == this.selected_ix;
                                                        div()
                                                            .w_full()
                                                            .h(px(row_height))
                                                            .px_1()
                                                            .text_size(TextSize::Default.rems())
                                                            .cursor_pointer()
                                                            .on_mouse_down(
                                                                MouseButton::Left,
                                                                cx.listener(
                                                                    move |this, _, window, cx| {
                                                                        this.selected_ix = match_ix;
                                                                        this.confirm_selected(
                                                                            window, cx,
                                                                        );
                                                                        cx.stop_propagation();
                                                                    },
                                                                ),
                                                            )
                                                            .child(
                                                                div()
                                                                    .flex()
                                                                    .flex_row()
                                                                    .items_center()
                                                                    .justify_between()
                                                                    .size_full()
                                                                    .rounded(px(RADIUS_MD))
                                                                    .px_2()
                                                                    .bg(if selected {
                                                                        rgb(theme.element_selected)
                                                                    } else {
                                                                        rgba(0x00000000)
                                                                    })
                                                                    .when(!selected, |this| {
                                                                        this.hover(move |style| {
                                                                            style
                                                                                .bg(rgb(theme
                                                                                    .element_hover))
                                                                        })
                                                                    })
                                                                    .child(
                                                                        this.delegate.render_match(
                                                                            match_ix,
                                                                            theme,
                                                                            &previous_focus_handle,
                                                                            window,
                                                                        ),
                                                                    ),
                                                            )
                                                    })
                                                    .collect::<Vec<_>>()
                                            },
                                        ),
                                    )
                                    .track_scroll(&scroll_handle)
                                    .with_decoration(uniform_list_scrollbar_for(
                                        &scroll_handle,
                                        "picker-matches-scrollbar",
                                        window,
                                        cx,
                                    ))
                                    .size_full(),
                                ),
                        )
                    }),
            )
    }
}

pub fn offset_from_utf16_in_text(text: &str, offset: usize) -> usize {
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

pub fn fuzzy_match(candidate: &str, query: &str) -> Option<(Vec<Range<usize>>, usize)> {
    if query.is_empty() {
        return Some((Vec::new(), 0));
    }

    let candidate_chars = candidate.char_indices().collect::<Vec<_>>();
    if candidate_chars.is_empty() {
        return None;
    }

    let mut states = vec![None::<MatchState>; candidate_chars.len()];

    for (query_ix, query_ch) in query.chars().map(|ch| ch.to_ascii_lowercase()).enumerate() {
        let mut next_states = vec![None::<MatchState>; candidate_chars.len()];

        if query_ix == 0 {
            for (char_ix, (byte_ix, candidate_ch)) in candidate_chars.iter().copied().enumerate() {
                if candidate_ch.to_ascii_lowercase() == query_ch {
                    let mut highlights = Vec::with_capacity(query.len());
                    highlights.push(byte_ix..byte_ix + candidate_ch.len_utf8());
                    let state = MatchState {
                        highlights,
                        score: fuzzy_match_score(candidate, char_ix, byte_ix, None),
                    };
                    replace_state_if_better(&mut next_states[char_ix], state);
                }
            }
        } else {
            for (previous_ix, state) in states.iter().enumerate() {
                let Some(state) = state else {
                    continue;
                };

                for (char_ix, (byte_ix, candidate_ch)) in candidate_chars
                    .iter()
                    .copied()
                    .enumerate()
                    .skip(previous_ix + 1)
                {
                    if candidate_ch.to_ascii_lowercase() != query_ch {
                        continue;
                    }

                    let mut next_state = state.clone();
                    next_state.score +=
                        fuzzy_match_score(candidate, char_ix, byte_ix, Some(previous_ix));
                    next_state
                        .highlights
                        .push(byte_ix..byte_ix + candidate_ch.len_utf8());
                    replace_state_if_better(&mut next_states[char_ix], next_state);
                }
            }
        }

        if next_states.iter().all(Option::is_none) {
            return None;
        }

        states = next_states;
    }

    states
        .into_iter()
        .flatten()
        .min_by_key(|state| state.score)
        .map(|state| (state.highlights, state.score))
}

fn replace_state_if_better(slot: &mut Option<MatchState>, state: MatchState) {
    if slot
        .as_ref()
        .is_none_or(|current| state.score < current.score)
    {
        *slot = Some(state);
    }
}

#[derive(Clone)]
struct MatchState {
    highlights: Vec<Range<usize>>,
    score: usize,
}

fn fuzzy_match_score(
    candidate: &str,
    char_ix: usize,
    byte_ix: usize,
    last_match_ix: Option<usize>,
) -> usize {
    let mut score = match last_match_ix {
        Some(previous_ix) => (char_ix - previous_ix - 1) * 2,
        None => char_ix,
    };

    if is_word_start(candidate, byte_ix) {
        score = score.saturating_sub(10);
    } else {
        score += 6;
    }

    if last_match_ix.is_some_and(|previous_ix| char_ix == previous_ix + 1) {
        score = score.saturating_sub(4);
    }

    score
}

fn is_word_start(text: &str, byte_ix: usize) -> bool {
    if byte_ix == 0 {
        return true;
    }

    let previous = text[..byte_ix].chars().next_back();
    let current = text[byte_ix..].chars().next();

    matches!(
        (previous, current),
        (Some(' ' | ':' | '_' | '-'), Some(ch)) if ch.is_alphanumeric()
    )
}

pub fn normalize_query(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut last_char: Option<char> = None;

    for char in input.trim().chars() {
        match (last_char, char) {
            (Some(last_char), char) if last_char.is_whitespace() && char.is_whitespace() => {
                continue;
            }
            _ => {
                last_char = Some(char);
            }
        }
        result.push(char);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_match_returns_utf8_ranges() {
        let (highlights, _) = fuzzy_match("open café", "cé").unwrap();
        assert_eq!(highlights, vec![5..6, 8..10]);
    }

    #[test]
    fn fuzzy_match_prefers_word_starts() {
        let (_, word_start_score) = fuzzy_match("command palette: toggle", "pt").unwrap();
        let (_, middle_score) = fuzzy_match("capture total", "pt").unwrap();
        assert!(word_start_score < middle_score);
    }
}
