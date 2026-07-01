use crate::{
    ai_client::{AiModelInfo, list_ai_models, test_ai_connection},
    input, keybindings,
    scrollbar::{WithScrollbar as _, uniform_list_scrollbar_for},
    settings::{
        AiModel, AiProvider, AiProviderConfig, AiThinkingLevel, FontSettings, KeymapPreset,
        ModelRef, ResolvedAiConfig, Settings, SettingsStore, TabBarVisibility,
        WindowBackgroundMode, setup_ui_font, ui_rems,
    },
    theme::{PartialTheme, Theme, ThemeMetadata, format_rgb_hex, format_rgba_hex},
};
use gpui::{
    Anchor, AnyElement, App, AppContext, Bounds, ClipboardItem, Context, CursorStyle, Element,
    ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, GlobalElementId, InspectorElementId, IntoElement, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point as GpuiPoint, Render,
    ScrollHandle, SharedString, Style, Task, TextRun, TitlebarOptions, UTF16Selection,
    UniformListScrollHandle, Window, WindowBounds, WindowOptions, anchored, deferred, div, fill,
    point, prelude::*, px, relative, rgb, rgba, size, uniform_list,
};
use std::{ops::Range, path::Path};

gpui::actions!(settings_panel, [PanelClose]);

// ---- inline text input used in the AI settings tab ----

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
    drag_origin: Option<GpuiPoint<Pixels>>,
}

const MASK_GLYPH: &str = "•";
const DRAG_SELECT_THRESHOLD: f32 = 2.0;

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

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
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
            self.select_to(self.index_for_mouse_position(event.position), cx);
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
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        self.content_offset_for_display_offset(line.closest_index_for_x(position.x - bounds.left()))
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
        Some(Bounds::from_corners(
            point(bounds.left() + line.x_for_index(range.start), bounds.top()),
            point(bounds.left() + line.x_for_index(range.end), bounds.bottom()),
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
        let utf8_index = line.index_for_x(pt.x - bounds.left())?;
        Some(self.offset_to_utf16(utf8_index))
    }
}

impl EventEmitter<SettingsInputEvent> for SettingsTextInput {}

impl Focusable for SettingsTextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
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
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(SettingsInputElement { input: entity })
    }
}

struct SettingsInputElement {
    input: Entity<SettingsTextInput>,
}

struct SettingsInputPrepaint {
    line: Option<gpui::ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for SettingsInputElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for SettingsInputElement {
    type RequestLayoutState = ();
    type PrepaintState = SettingsInputPrepaint;

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
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> SettingsInputPrepaint {
        let input = self.input.read(cx);
        let theme = cx.global::<SettingsStore>().resolved_theme;
        let style = window.text_style();

        let display_text: SharedString = if input.mask && !input.content.is_empty() {
            MASK_GLYPH.repeat(input.content.chars().count()).into()
        } else {
            input.content.clone()
        };

        let (display_text, text_color) = if display_text.is_empty() {
            (
                input.placeholder.clone(),
                rgba(theme.placeholder_text).into(),
            )
        } else {
            (display_text, style.color)
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };

        let runs = if !input.content.is_empty() {
            if let Some(marked_range) = input.marked_range.as_ref() {
                let marked_range = input.display_range(marked_range);
                vec![
                    TextRun {
                        len: marked_range.start,
                        ..run.clone()
                    },
                    TextRun {
                        len: marked_range.end - marked_range.start,
                        underline: Some(gpui::UnderlineStyle {
                            color: Some(run.color),
                            thickness: px(1.0),
                            wavy: false,
                        }),
                        ..run.clone()
                    },
                    TextRun {
                        len: display_text.len() - marked_range.end,
                        ..run
                    },
                ]
                .into_iter()
                .filter(|r| r.len > 0)
                .collect()
            } else {
                vec![run]
            }
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let cursor = input.cursor_offset();
        let display_cursor = input.display_offset(cursor);
        let cursor_pos = line.x_for_index(display_cursor);
        let selected_range = &input.selected_range;

        let (selection, cursor_quad) = if selected_range.is_empty() {
            let quad = fill(
                Bounds::new(
                    point(bounds.left() + cursor_pos, bounds.top()),
                    size(px(2.0), bounds.bottom() - bounds.top()),
                ),
                rgb(theme.accent),
            );
            (None, Some(quad))
        } else {
            let selected_range = input.display_range(selected_range);
            let quad = fill(
                Bounds::from_corners(
                    point(
                        bounds.left() + line.x_for_index(selected_range.start),
                        bounds.top(),
                    ),
                    point(
                        bounds.left() + line.x_for_index(selected_range.end),
                        bounds.bottom(),
                    ),
                ),
                rgba(theme.selection_highlight),
            );
            (Some(quad), None)
        };

        SettingsInputPrepaint {
            line: Some(line),
            cursor: cursor_quad,
            selection,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut SettingsInputPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );

        if let Some(sel) = prepaint.selection.take() {
            window.paint_quad(sel);
        }

        if let Some(line) = prepaint.line.take() {
            if let Err(e) = line.paint(
                bounds.origin,
                window.line_height(),
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            ) {
                eprintln!("settings input paint error: {e:#}");
            }

            if focus_handle.is_focused(window)
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }

            self.input.update(cx, |input, _| {
                input.last_layout = Some(line);
                input.last_bounds = Some(bounds);
            });
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

// ---- settings panel ----

const NUMBER_STEP_SMALL: f32 = 0.05;
const OPACITY_STEP: f32 = 0.05;
const PAGE_GAP_STEP: f32 = 4.0;
const FONT_SIZE_STEP: f32 = 1.0;
const FONT_WEIGHT_STEP: f32 = 100.0;
const COLOR_SWATCHES: [u32; 10] = [
    0x101010, 0x181818, 0x202020, 0x282c34, 0x1e1e2e, 0x313244, 0x61afef, 0x89b4fa, 0xf5f5f5,
    0xffffff,
];
const HIGHLIGHT_SWATCHES: [u32; 6] = [
    0x61afef66, 0x89b4fa66, 0xc678dd66, 0xe5c07b66, 0x98c37966, 0xe06c7566,
];
const ALPHA_SWATCHES: [u32; 6] = [
    0x00000033, 0x00000066, 0xffffff66, 0x61afef55, 0x3a3f4bcc, 0x45475a99,
];

struct ColorRowConfig {
    label: &'static str,
    current: u32,
    override_value: Option<u32>,
    swatches: &'static [u32],
    with_alpha: bool,
}

pub enum PanelEvent {
    Dismissed,
    Changed,
}

pub fn open_settings_window(cx: &mut App) {
    open_settings_window_with_section(None, cx);
}

fn open_settings_window_with_section(section: Option<SettingsSection>, cx: &mut App) {
    if let Some(existing_window) = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<SettingsWindow>())
    {
        if let Err(error) = existing_window.update(cx, move |settings, window, cx| {
            if let Some(section) = section {
                settings.active_tab = section.tab();
                settings.active_section = section;
                cx.notify();
            }
            window.activate_window();
        }) {
            eprintln!("failed to activate settings window: {error:#}");
        }
        return;
    }

    let window_settings = cx.global::<SettingsStore>().settings.window;
    let transparency_enabled = window_settings.transparency_enabled_by_default();
    let bounds = Bounds::centered(None, size(px(900.0), px(760.0)), cx);

    if let Err(error) = cx.open_window(
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("Glyphst Settings".into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_background: window_settings.background_appearance(transparency_enabled),
            app_id: Some("glyphst".to_string()),
            window_min_size: Some(size(px(720.0), px(420.0))),
            ..Default::default()
        },
        move |window, cx| {
            window.set_app_id("glyphst");
            cx.new(|cx| {
                let mut settings = SettingsWindow::new(cx);
                if let Some(section) = section {
                    settings.active_tab = section.tab();
                    settings.active_section = section;
                }
                settings
            })
        },
    ) {
        eprintln!("failed to open settings window: {error:#}");
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    General,
    Keymap,
    Appearance,
    AiGeneral,
    AiProviders,
}

impl SettingsTab {
    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Keymap => "Keymap",
            Self::Appearance => "Appearance",
            Self::AiGeneral => "General",
            Self::AiProviders => "Model Providers",
        }
    }

    fn first_section(self) -> SettingsSection {
        SettingsSection::for_tab(self)[0]
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsSection {
    GeneralZoom,
    GeneralScroll,
    GeneralKeymap,
    GeneralWindow,
    GeneralTabs,
    AppearanceTheme,
    AppearanceFonts,
    AppearanceOverrides,
    AiGeneral,
    AiProviders,
}

const GENERAL_SECTIONS: [SettingsSection; 4] = [
    SettingsSection::GeneralZoom,
    SettingsSection::GeneralScroll,
    SettingsSection::GeneralWindow,
    SettingsSection::GeneralTabs,
];
const KEYMAP_SECTIONS: [SettingsSection; 1] = [SettingsSection::GeneralKeymap];
const APPEARANCE_SECTIONS: [SettingsSection; 3] = [
    SettingsSection::AppearanceTheme,
    SettingsSection::AppearanceFonts,
    SettingsSection::AppearanceOverrides,
];
const AI_GENERAL_SECTIONS: [SettingsSection; 1] = [SettingsSection::AiGeneral];
const AI_PROVIDER_SECTIONS: [SettingsSection; 1] = [SettingsSection::AiProviders];

impl SettingsSection {
    fn tab(self) -> SettingsTab {
        match self {
            Self::GeneralZoom | Self::GeneralScroll | Self::GeneralWindow | Self::GeneralTabs => {
                SettingsTab::General
            }
            Self::GeneralKeymap => SettingsTab::Keymap,
            Self::AppearanceTheme | Self::AppearanceFonts | Self::AppearanceOverrides => {
                SettingsTab::Appearance
            }
            Self::AiGeneral => SettingsTab::AiGeneral,
            Self::AiProviders => SettingsTab::AiProviders,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::GeneralZoom => "Zoom",
            Self::GeneralScroll => "Scroll",
            Self::GeneralKeymap => "Keymap",
            Self::GeneralWindow => "Window",
            Self::GeneralTabs => "Tabs",
            Self::AppearanceTheme => "Theme",
            Self::AppearanceFonts => "Fonts",
            Self::AppearanceOverrides => "Overrides",
            Self::AiGeneral => "General",
            Self::AiProviders => "Model Providers",
        }
    }

    fn for_tab(tab: SettingsTab) -> &'static [Self] {
        match tab {
            SettingsTab::General => &GENERAL_SECTIONS,
            SettingsTab::Keymap => &KEYMAP_SECTIONS,
            SettingsTab::Appearance => &APPEARANCE_SECTIONS,
            SettingsTab::AiGeneral => &AI_GENERAL_SECTIONS,
            SettingsTab::AiProviders => &AI_PROVIDER_SECTIONS,
        }
    }

    fn index_in_tab(self) -> usize {
        Self::for_tab(self.tab())
            .iter()
            .position(|section| *section == self)
            .unwrap_or(0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProviderDropdownTarget {
    Main,
    Title,
}

#[derive(Clone, PartialEq, Eq)]
struct ModelRequestSignature {
    provider: AiProvider,
    base_url: String,
    has_api_key: bool,
}

struct ModelPickerState {
    open: bool,
    query: SharedString,
    models: Vec<AiModelInfo>,
    loading: bool,
    status: Option<SharedString>,
    request: Option<ModelRequestSignature>,
    task: Option<Task<()>>,
    anchor: Option<GpuiPoint<Pixels>>,
    scroll_handle: ScrollHandle,
    fetch_scroll_handle: UniformListScrollHandle,
}

impl Default for ModelPickerState {
    fn default() -> Self {
        Self {
            open: false,
            query: SharedString::default(),
            models: Vec::new(),
            loading: false,
            status: None,
            request: None,
            task: None,
            anchor: None,
            scroll_handle: ScrollHandle::new(),
            fetch_scroll_handle: UniformListScrollHandle::new(),
        }
    }
}

struct FontPickerState {
    open: bool,
    anchor: Option<GpuiPoint<Pixels>>,
    fonts: Vec<String>,
    scroll_handle: ScrollHandle,
}

impl FontPickerState {
    fn new(cx: &App, font_settings: &FontSettings) -> Self {
        let mut fonts = cx.text_system().all_font_names();
        fonts.extend(
            [".ZedSans", ".ZedMono", ".SystemUIFont"]
                .into_iter()
                .map(str::to_string),
        );
        fonts.push(font_settings.ui_font_family.clone());
        fonts.extend(font_settings.ui_font_fallbacks.clone());
        fonts.extend(FontSettings::default().ui_font_fallbacks);
        fonts.sort_by_cached_key(|font| font.to_ascii_lowercase());
        fonts.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

        Self {
            open: false,
            anchor: None,
            fonts,
            scroll_handle: ScrollHandle::new(),
        }
    }
}

#[derive(Clone)]
enum SettingsModelMenuEntry {
    Header(AiProvider),
    Model(AiProvider, AiModel),
}

pub struct SettingsWindow {
    focus_handle: FocusHandle,
    active_tab: SettingsTab,
    active_section: SettingsSection,
    ai_expanded: bool,
    content_scroll: ScrollHandle,
    status: Option<SharedString>,
    save_error: Option<SharedString>,
    selected_ai_provider: AiProvider,
    main_model_picker: ModelPickerState,
    title_model_picker: ModelPickerState,
    provider_model_picker: ModelPickerState,
    provider_model_selection: Vec<String>,
    font_picker: FontPickerState,
    font_search_input: Entity<SettingsTextInput>,
    ui_font_fallbacks_input: Entity<SettingsTextInput>,
    ai_model_input: Entity<SettingsTextInput>,
    ai_key_input: Entity<SettingsTextInput>,
    ai_base_url_input: Entity<SettingsTextInput>,
    main_model_search: Entity<SettingsTextInput>,
    title_model_search: Entity<SettingsTextInput>,
    fetch_model_search: Entity<SettingsTextInput>,
}

impl SettingsWindow {
    fn font_fallbacks_input_value(font: &FontSettings) -> SharedString {
        font.ui_font_fallbacks.join(", ").into()
    }

    fn parse_font_fallbacks_input(value: &str) -> Vec<String> {
        value
            .split(',')
            .map(str::trim)
            .filter(|fallback| !fallback.is_empty())
            .map(str::to_string)
            .collect()
    }

    pub fn new(cx: &mut Context<Self>) -> Self {
        let (provider, config, font_settings) = {
            let settings = &cx.global::<SettingsStore>().settings;
            let ai = &settings.ai;
            let provider = ai
                .main_model
                .as_ref()
                .map(|model_ref| model_ref.provider)
                .unwrap_or(AiProvider::OpenAi);
            (
                provider,
                ai.provider_config(provider),
                settings.font.clone(),
            )
        };

        let font_picker = FontPickerState::new(cx, &font_settings);
        let font_search_input =
            cx.new(|cx| SettingsTextInput::new("".into(), "Search fonts…".into(), false, cx));
        cx.subscribe(&font_search_input, |_, _, _, cx| cx.notify())
            .detach();

        let ui_font_fallbacks_input = cx.new(|cx| {
            SettingsTextInput::new(
                Self::font_fallbacks_input_value(&font_settings),
                "Noto Sans CJK SC, Source Han Sans SC".into(),
                false,
                cx,
            )
        });
        cx.subscribe(&ui_font_fallbacks_input, |this, _, event, cx| {
            let SettingsInputEvent::Changed(value) = event;
            let fallbacks = Self::parse_font_fallbacks_input(value.as_ref());
            this.update_font_settings(move |font| font.ui_font_fallbacks = fallbacks, cx);
        })
        .detach();

        let ai_model_input =
            cx.new(|cx| SettingsTextInput::new("".into(), "model id…".into(), false, cx));
        let ai_key_input = cx.new(|cx| {
            SettingsTextInput::new(
                config.api_key.clone().into(),
                "paste API key…".into(),
                true,
                cx,
            )
        });
        let ai_base_url_input = cx.new(|cx| {
            SettingsTextInput::new(
                config
                    .base_url
                    .clone()
                    .unwrap_or_else(|| provider.default_base_url().to_string())
                    .into(),
                provider.default_base_url().into(),
                false,
                cx,
            )
        });
        cx.subscribe(&ai_key_input, |this, _, event, cx| {
            let SettingsInputEvent::Changed(value) = event;
            let value = value.to_string();
            let provider = this.selected_ai_provider;
            this.update_provider_config(provider, move |_, config| config.api_key = value, cx);
        })
        .detach();

        let main_model_search =
            cx.new(|cx| SettingsTextInput::new("".into(), "Search model…".into(), false, cx));
        let title_model_search =
            cx.new(|cx| SettingsTextInput::new("".into(), "Search model…".into(), false, cx));
        cx.subscribe(&main_model_search, |this, _, event, cx| {
            let SettingsInputEvent::Changed(value) = event;
            this.main_model_picker.query = value.clone();
            cx.notify();
        })
        .detach();
        cx.subscribe(&title_model_search, |this, _, event, cx| {
            let SettingsInputEvent::Changed(value) = event;
            this.title_model_picker.query = value.clone();
            cx.notify();
        })
        .detach();

        let fetch_model_search =
            cx.new(|cx| SettingsTextInput::new("".into(), "Search model…".into(), false, cx));
        cx.subscribe(&fetch_model_search, |this, _, event, cx| {
            let SettingsInputEvent::Changed(value) = event;
            this.provider_model_picker.query = value.clone();
            cx.notify();
        })
        .detach();

        cx.subscribe(&ai_base_url_input, |this, _, event, cx| {
            let SettingsInputEvent::Changed(value) = event;
            let value = value.to_string();
            let provider = this.selected_ai_provider;
            this.update_provider_config(
                provider,
                move |provider, config| {
                    let trimmed = value.trim().trim_end_matches('/').to_string();
                    config.base_url =
                        if trimmed.is_empty() || trimmed == provider.default_base_url() {
                            None
                        } else {
                            Some(trimmed)
                        };
                },
                cx,
            );
        })
        .detach();

        cx.observe_global::<SettingsStore>(|_, cx| cx.notify())
            .detach();

        Self {
            focus_handle: cx.focus_handle(),
            active_tab: SettingsTab::General,
            active_section: SettingsSection::GeneralZoom,
            ai_expanded: true,
            content_scroll: ScrollHandle::new(),
            status: None,
            save_error: None,
            selected_ai_provider: provider,
            main_model_picker: ModelPickerState::default(),
            title_model_picker: ModelPickerState::default(),
            provider_model_picker: ModelPickerState::default(),
            provider_model_selection: Vec::new(),
            font_picker,
            font_search_input,
            ui_font_fallbacks_input,
            ai_model_input,
            ai_key_input,
            ai_base_url_input,
            main_model_search,
            title_model_search,
            fetch_model_search,
        }
    }

    fn persist_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        let result = cx.global_mut::<SettingsStore>().set_settings(settings);
        let (keymap_preset, keybinding_overrides) = {
            let store = cx.global::<SettingsStore>();
            (
                store.settings.keymap_preset,
                store.settings.keybinding_overrides.clone(),
            )
        };
        keybindings::apply(cx, keymap_preset, &keybinding_overrides);
        cx.refresh_windows();
        self.status = None;
        self.save_error = result.err().map(|e| format!("{e:#}").into());
        cx.emit(PanelEvent::Changed);
        cx.notify();
    }

    fn base_url_input_content(provider: AiProvider, config: &AiProviderConfig) -> SharedString {
        config
            .base_url
            .clone()
            .unwrap_or_else(|| provider.default_base_url().to_string())
            .into()
    }

    fn update_provider_config(
        &mut self,
        provider: AiProvider,
        update: impl FnOnce(AiProvider, &mut AiProviderConfig),
        cx: &mut Context<Self>,
    ) {
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        update(provider, settings.ai.provider_config_mut(provider));
        self.persist_settings(settings, cx);
    }

    fn update_font_settings(
        &mut self,
        update: impl FnOnce(&mut FontSettings),
        cx: &mut Context<Self>,
    ) {
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        update(&mut settings.font);
        self.persist_settings(settings, cx);
    }

    fn model_picker(&self, target: ProviderDropdownTarget) -> &ModelPickerState {
        match target {
            ProviderDropdownTarget::Main => &self.main_model_picker,
            ProviderDropdownTarget::Title => &self.title_model_picker,
        }
    }

    fn model_picker_mut(&mut self, target: ProviderDropdownTarget) -> &mut ModelPickerState {
        match target {
            ProviderDropdownTarget::Main => &mut self.main_model_picker,
            ProviderDropdownTarget::Title => &mut self.title_model_picker,
        }
    }

    fn model_search_input(&self, target: ProviderDropdownTarget) -> &Entity<SettingsTextInput> {
        match target {
            ProviderDropdownTarget::Main => &self.main_model_search,
            ProviderDropdownTarget::Title => &self.title_model_search,
        }
    }

    fn resolved_ai_config_for_provider(
        settings: &Settings,
        provider: AiProvider,
    ) -> ResolvedAiConfig {
        let config = settings.ai.provider_config(provider);
        let model = config
            .models
            .first()
            .map(|model| model.id.clone())
            .unwrap_or_else(|| provider.default_model().to_string());
        let base_url_overridden = config.base_url.is_some();
        ResolvedAiConfig {
            provider,
            model,
            api_key: config.api_key,
            base_url: config
                .base_url
                .unwrap_or_else(|| provider.default_base_url().to_string()),
            base_url_overridden,
            thinking_level: AiThinkingLevel::Off,
        }
    }

    fn model_request_signature(config: &ResolvedAiConfig) -> ModelRequestSignature {
        ModelRequestSignature {
            provider: config.provider,
            base_url: config.base_url.clone(),
            has_api_key: !config.api_key.is_empty(),
        }
    }

    fn current_provider_request_signature(
        &self,
        provider: AiProvider,
        cx: &mut Context<Self>,
    ) -> ModelRequestSignature {
        let settings = cx.global::<SettingsStore>().settings.clone();
        let config = Self::resolved_ai_config_for_provider(&settings, provider);
        Self::model_request_signature(&config)
    }

    fn select_model(
        &mut self,
        target: ProviderDropdownTarget,
        provider: AiProvider,
        model: String,
        cx: &mut Context<Self>,
    ) {
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        let model_ref = Some(ModelRef::new(provider, model));
        match target {
            ProviderDropdownTarget::Main => {
                settings.ai.main_model = model_ref;
                self.main_model_picker.open = false;
                self.main_model_picker.anchor = None;
                self.main_model_picker.query = SharedString::default();
            }
            ProviderDropdownTarget::Title => {
                settings.ai.title_model = model_ref;
                self.title_model_picker.open = false;
                self.title_model_picker.anchor = None;
                self.title_model_picker.query = SharedString::default();
            }
        }
        self.model_search_input(target).update(cx, |input, cx| {
            input.set_content(SharedString::default(), cx);
        });
        self.persist_settings(settings, cx);
    }

    fn fetch_provider_models(
        &mut self,
        provider: AiProvider,
        anchor: Option<GpuiPoint<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let settings = cx.global::<SettingsStore>().settings.clone();
        let config = Self::resolved_ai_config_for_provider(&settings, provider);
        let signature = Self::model_request_signature(&config);
        {
            let picker = &mut self.provider_model_picker;
            picker.loading = true;
            picker.status = Some("Fetching models…".into());
            picker.request = Some(signature.clone());
            picker.open = false;
            picker.anchor = anchor;
            picker.query = SharedString::default();
        }
        self.provider_model_selection.clear();
        self.fetch_model_search.update(cx, |input, cx| {
            input.set_content(SharedString::default(), cx);
        });

        self.provider_model_picker.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    list_ai_models(config).map_err(|error| format!("{error:#}"))
                })
                .await;
            _ = this.update(cx, |this, cx| {
                if this.current_provider_request_signature(provider, cx) != signature {
                    return;
                }
                let picker = &mut this.provider_model_picker;
                picker.loading = false;
                match result {
                    Ok(models) => {
                        let count = models.len();
                        picker.models = models;
                        picker.status = Some(format!("Fetched {count} models.").into());
                        picker.open = true;
                    }
                    Err(error) => {
                        picker.status = Some(format!("Failed to fetch models: {error}").into());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn test_connection(&mut self, provider: AiProvider, cx: &mut Context<Self>) {
        let settings = cx.global::<SettingsStore>().settings.clone();
        let config = Self::resolved_ai_config_for_provider(&settings, provider);
        let signature = Self::model_request_signature(&config);
        {
            let picker = &mut self.provider_model_picker;
            picker.loading = true;
            picker.status = Some("Testing connection…".into());
            picker.request = Some(signature.clone());
        }

        self.provider_model_picker.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    test_ai_connection(config).map_err(|error| format!("{error:#}"))
                })
                .await;
            _ = this.update(cx, |this, cx| {
                if this.current_provider_request_signature(provider, cx) != signature {
                    return;
                }
                let picker = &mut this.provider_model_picker;
                picker.loading = false;
                picker.status = Some(match result {
                    Ok(()) => "Connection succeeded.".into(),
                    Err(error) => format!("Connection failed: {error}").into(),
                });
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn sync_provider_inputs(
        &mut self,
        provider: AiProvider,
        config: AiProviderConfig,
        cx: &mut Context<Self>,
    ) {
        self.ai_model_input.update(cx, |input, cx| {
            input.placeholder = "model id…".into();
            input.set_content("".into(), cx);
        });
        self.ai_key_input.update(cx, |input, cx| {
            input.placeholder = "paste API key…".into();
            input.set_content(config.api_key.clone().into(), cx);
        });
        self.ai_base_url_input.update(cx, |input, cx| {
            input.placeholder = provider.default_base_url().into();
            input.set_content(Self::base_url_input_content(provider, &config), cx);
        });
    }

    fn select_ai_provider(&mut self, provider: AiProvider, cx: &mut Context<Self>) {
        self.selected_ai_provider = provider;
        let settings = cx.global::<SettingsStore>().settings.clone();
        self.sync_provider_inputs(provider, settings.ai.provider_config(provider), cx);
        self.provider_model_picker.open = false;
        self.provider_model_picker.anchor = None;
        self.provider_model_selection.clear();
        cx.notify();
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.remove_window();
        cx.emit(PanelEvent::Dismissed);
    }

    fn close(&mut self, _: &PanelClose, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(window, cx);
    }

    fn change_settings(
        &mut self,
        update: impl FnOnce(&mut Settings),
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        update(&mut settings);
        self.persist_settings(settings, cx);
    }

    fn export(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let config_path = cx.global::<SettingsStore>().config_path.clone();
        let directory = config_path
            .parent()
            .map(Path::to_path_buf)
            .filter(|path| path.exists())
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let dialog = cx.prompt_for_new_path(&directory, Some("config.kdl"));

        cx.spawn(async move |this, cx| {
            let result = dialog.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(Some(path))) => match cx.global::<SettingsStore>().export_to(&path) {
                        Ok(()) => {
                            this.save_error = None;
                            this.status = Some(format!("Exported to {}", path.display()).into());
                        }
                        Err(error) => {
                            this.status = None;
                            this.save_error = Some(format!("{error:#}").into());
                        }
                    },
                    Ok(Err(error)) => {
                        this.status = None;
                        this.save_error = Some(format!("{error:#}").into());
                    }
                    Ok(Ok(None)) | Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn settings_nav(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut entries = vec![
            self.nav_root_entry(SettingsTab::General, theme, cx),
            self.nav_root_entry(SettingsTab::Keymap, theme, cx),
            self.nav_root_entry(SettingsTab::Appearance, theme, cx),
            self.nav_ai_parent_entry(theme, cx),
        ];
        if self.ai_expanded {
            entries.push(self.nav_ai_children(theme, cx));
        }

        div()
            .w(px(220.0))
            .flex_none()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .border_r(px(1.0))
            .border_color(rgb(theme.border))
            .bg(rgb(theme.panel_background))
            .children(entries)
    }

    fn nav_ai_children(&self, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let entries = [SettingsSection::AiGeneral, SettingsSection::AiProviders]
            .into_iter()
            .map(|section| self.nav_sub_entry(section, theme, cx))
            .collect::<Vec<_>>();

        div()
            .w_full()
            .flex()
            .flex_row()
            .child(
                div()
                    .w(px(28.0))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .child(div().w(px(1.0)).h_full().bg(rgb(theme.border_variant))),
            )
            .child(div().flex_1().flex().flex_col().children(entries))
            .into_any_element()
    }

    fn nav_root_entry(&self, tab: SettingsTab, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.active_tab == tab;

        div()
            .w_full()
            .h(px(28.0))
            .px_2()
            .flex()
            .flex_row()
            .items_center()
            .rounded(px(4.0))
            .border_1()
            .border_color(if selected {
                rgb(theme.border_variant)
            } else {
                rgba(0x00000000)
            })
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .text_color(rgb(if selected {
                theme.text
            } else {
                theme.text_muted
            }))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.active_tab = tab;
                    this.active_section = tab.first_section();
                    this.content_scroll.scroll_to_top_of_item(0);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(tab.label())
            .into_any_element()
    }

    fn nav_ai_parent_entry(&self, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let selected = matches!(
            self.active_tab,
            SettingsTab::AiGeneral | SettingsTab::AiProviders
        );
        let expanded = self.ai_expanded;

        div()
            .w_full()
            .h(px(28.0))
            .px_1()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .rounded(px(4.0))
            .border_1()
            .border_color(if selected {
                rgb(theme.border_variant)
            } else {
                rgba(0x00000000)
            })
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .text_color(rgb(if selected {
                theme.text
            } else {
                theme.text_muted
            }))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.ai_expanded = !this.ai_expanded;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(16.0))
                    .flex_none()
                    .text_color(rgb(theme.text_muted))
                    .child(if expanded { "▾" } else { "▸" }),
            )
            .child("AI")
            .into_any_element()
    }

    fn nav_sub_entry(
        &self,
        section: SettingsSection,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.active_section == section;
        let tab = section.tab();
        let section_index = section.index_in_tab();

        div()
            .w_full()
            .h(px(28.0))
            .flex()
            .flex_row()
            .items_center()
            .rounded(px(4.0))
            .border_1()
            .border_color(if selected {
                rgb(theme.border_variant)
            } else {
                rgba(0x00000000)
            })
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .text_color(rgb(if selected {
                theme.text
            } else {
                theme.text_muted
            }))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.active_tab = tab;
                    this.active_section = section;
                    this.ai_expanded = true;
                    this.content_scroll.scroll_to_top_of_item(section_index);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(div().px_1().child(section.label()))
            .into_any_element()
    }

    fn number_row(
        &self,
        label: &'static str,
        value: f32,
        decrement: impl FnOnce(&mut Settings) + Clone + 'static,
        increment: impl FnOnce(&mut Settings) + Clone + 'static,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(44.0))
            .child(div().text_color(rgb(theme.text)).child(label))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(self.small_button("−", decrement, theme, cx))
                    .child(
                        div()
                            .w(px(72.0))
                            .text_center()
                            .text_color(rgb(theme.text_muted))
                            .child(format!("{value:.2}")),
                    )
                    .child(self.small_button("+", increment, theme, cx)),
            )
    }

    fn toggle_row(
        &self,
        label: &'static str,
        enabled: bool,
        update: impl FnOnce(&mut Settings) + Clone + 'static,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(44.0))
            .child(div().text_color(rgb(theme.text)).child(label))
            .child(
                div()
                    .h(px(28.0))
                    .min_w(px(56.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_1()
                    .border_color(rgb(theme.border_variant))
                    .bg(if enabled {
                        rgb(theme.element_selected)
                    } else {
                        rgb(theme.surface_background)
                    })
                    .text_color(rgb(if enabled {
                        theme.accent
                    } else {
                        theme.text_muted
                    }))
                    .hover(move |style| style.bg(rgb(theme.element_hover)))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.change_settings(update.clone(), window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(if enabled { "On" } else { "Off" }),
            )
    }

    fn text_input_row(
        &self,
        label: &'static str,
        input: Entity<SettingsTextInput>,
        theme: Theme,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .min_h(px(44.0))
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .w(px(132.0))
                    .text_color(rgb(theme.text))
                    .child(label),
            )
            .child(input)
    }

    fn small_button(
        &self,
        label: &'static str,
        update: impl FnOnce(&mut Settings) + Clone + 'static,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .h(px(28.0))
            .min_w(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.change_settings(update.clone(), window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn reset_font_settings_button(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(28.0))
            .min_w(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    let defaults = FontSettings::default();
                    this.font_picker.open = false;
                    this.font_picker.anchor = None;
                    this.font_search_input
                        .update(cx, |input, cx| input.set_content("".into(), cx));
                    this.ui_font_fallbacks_input.update(cx, |input, cx| {
                        input.set_content(Self::font_fallbacks_input_value(&defaults), cx);
                    });
                    this.change_settings(move |settings| settings.font = defaults, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child("Reset fonts")
    }

    fn color_row(
        &self,
        config: ColorRowConfig,
        update: impl Fn(&mut PartialTheme, u32) + Copy + 'static,
        reset: impl Fn(&mut PartialTheme) + Copy + 'static,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let value = if config.with_alpha {
            format_rgba_hex(config.current)
        } else {
            format_rgb_hex(config.current)
        };
        let source = if config.override_value.is_some() {
            "override"
        } else {
            "theme"
        };

        div()
            .flex()
            .flex_col()
            .py_2()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .mb_2()
                    .child(div().text_color(rgb(theme.text)).child(config.label))
                    .child(
                        div()
                            .text_color(rgb(theme.text_muted))
                            .child(format!("{value} · {source}")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .when(config.override_value.is_some(), |this| {
                        this.child(self.small_button(
                            "Reset",
                            move |settings| reset(&mut settings.overrides),
                            theme,
                            cx,
                        ))
                    })
                    .children(config.swatches.iter().copied().map(move |color| {
                        let selected = color == config.current;
                        div()
                            .w(px(28.0))
                            .h(px(28.0))
                            .border_2()
                            .border_color(if selected {
                                rgb(theme.text)
                            } else {
                                rgb(theme.border_variant)
                            })
                            .bg(if config.with_alpha {
                                rgba(color)
                            } else {
                                rgb(color)
                            })
                            .hover(move |style| style.border_color(rgb(theme.text)))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    this.change_settings(
                                        move |settings| update(&mut settings.overrides, color),
                                        window,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                }),
                            )
                    })),
            )
    }

    fn theme_option(
        &self,
        metadata: ThemeMetadata,
        selected_id: &str,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = metadata.id == selected_id;
        let id = metadata.id.clone();

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(38.0))
            .px_3()
            .border_1()
            .border_color(if selected {
                rgb(theme.accent)
            } else {
                rgb(theme.border_variant)
            })
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    let id = id.clone();
                    this.change_settings(move |settings| settings.theme_name = id, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(div().text_color(rgb(theme.text)).child(metadata.name))
            .child(
                div()
                    .text_color(rgb(theme.text_muted))
                    .child(metadata.appearance.as_str()),
            )
    }

    fn background_option(
        &self,
        label: &'static str,
        mode: WindowBackgroundMode,
        selected_mode: WindowBackgroundMode,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = mode == selected_mode;

        div()
            .h(px(30.0))
            .px_3()
            .flex()
            .items_center()
            .border_1()
            .border_color(if selected {
                rgb(theme.accent)
            } else {
                rgb(theme.border_variant)
            })
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.change_settings(
                        move |settings| settings.window.background = mode,
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn tab_bar_visibility_option(
        &self,
        label: &'static str,
        visibility: TabBarVisibility,
        selected_visibility: TabBarVisibility,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = visibility == selected_visibility;

        div()
            .h(px(30.0))
            .px_3()
            .flex()
            .items_center()
            .border_1()
            .border_color(if selected {
                rgb(theme.accent)
            } else {
                rgb(theme.border_variant)
            })
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.change_settings(
                        move |settings| settings.tab_bar_visibility = visibility,
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn keymap_option(
        &self,
        preset: KeymapPreset,
        selected_preset: KeymapPreset,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = preset == selected_preset;

        div()
            .h(px(30.0))
            .px_3()
            .flex()
            .items_center()
            .border_1()
            .border_color(if selected {
                rgb(theme.accent)
            } else {
                rgb(theme.border_variant)
            })
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.change_settings(
                        move |settings| settings.keymap_preset = preset,
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .child(preset.label())
    }

    fn keybinding_button(
        &self,
        label: &'static str,
        theme: Theme,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .h(px(28.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    on_click(this, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn font_picker_entries(&self, selected_family: &str, query: &str) -> Vec<String> {
        let query = query.trim().to_ascii_lowercase();
        let mut fonts = self.font_picker.fonts.clone();
        if !fonts
            .iter()
            .any(|font| font.eq_ignore_ascii_case(selected_family))
        {
            fonts.push(selected_family.to_string());
            fonts.sort_by_cached_key(|font| font.to_ascii_lowercase());
            fonts.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        }
        if !query.is_empty() {
            fonts.retain(|font| font.to_ascii_lowercase().contains(&query));
        }
        fonts
    }

    fn font_family_row(
        &self,
        settings: &Settings,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .min_h(px(44.0))
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .w(px(132.0))
                    .text_color(rgb(theme.text))
                    .child("UI font family"),
            )
            .child(self.font_family_control(settings, theme, cx))
    }

    fn font_family_control(
        &self,
        settings: &Settings,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let open = self.font_picker.open;
        let font_family = settings.font.ui_font_family.clone();

        div()
            .w(px(280.0))
            .h(px(32.0))
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .border_1()
            .border_color(rgb(if open {
                theme.accent
            } else {
                theme.border_variant
            }))
            .bg(rgb(theme.surface_background))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    let open = !this.font_picker.open;
                    this.main_model_picker.open = false;
                    this.main_model_picker.anchor = None;
                    this.title_model_picker.open = false;
                    this.title_model_picker.anchor = None;
                    this.font_picker.open = open;
                    this.font_picker.anchor = open
                        .then(|| point(event.position.x - px(8.0), event.position.y + px(20.0)));
                    this.font_search_input.update(cx, |input, cx| {
                        input.set_content("".into(), cx);
                    });
                    if open {
                        this.font_search_input
                            .read(cx)
                            .focus_handle
                            .clone()
                            .focus(window, cx);
                    }
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(div().flex_1().min_w_0().truncate().child(font_family))
            .child(
                div()
                    .flex_none()
                    .text_color(rgb(theme.text_muted))
                    .child(if open { "⌃" } else { "⌄" }),
            )
    }

    fn select_font_family(&mut self, family: String, cx: &mut Context<Self>) {
        self.font_picker.open = false;
        self.font_picker.anchor = None;
        self.font_search_input.update(cx, |input, cx| {
            input.set_content("".into(), cx);
        });
        self.update_font_settings(move |font| font.ui_font_family = family, cx);
    }

    fn font_picker_option(
        family: String,
        selected_family: String,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = family == selected_family;
        let family_for_click = family.clone();

        div()
            .h(px(30.0))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(4.0))
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .text_color(rgb(if selected { theme.accent } else { theme.text }))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.select_font_family(family_for_click.clone(), cx);
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .w(px(18.0))
                    .flex_none()
                    .text_color(rgb(theme.accent))
                    .child(if selected { "✓" } else { "" }),
            )
            .child(div().flex_1().min_w_0().truncate().child(family))
            .into_any_element()
    }

    fn font_picker_menu(
        &self,
        settings: &Settings,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = settings.font.ui_font_family.clone();
        let query = self.font_search_input.read(cx).content.to_string();
        let fonts = self.font_picker_entries(&selected, &query);
        let entry_count = fonts.len();
        let scroll_handle = self.font_picker.scroll_handle.clone();
        let search_input = self.font_search_input.clone();
        let rows = fonts
            .into_iter()
            .map(|font| Self::font_picker_option(font, selected.clone(), theme, cx))
            .collect::<Vec<_>>();

        div()
            .w(px(280.0))
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .shadow_sm()
            .overflow_hidden()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .child(
                div()
                    .px_2()
                    .py_2()
                    .border_b(px(1.0))
                    .border_color(rgb(theme.border_variant))
                    .flex()
                    .child(search_input),
            )
            .when(entry_count == 0, |this| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_color(rgb(theme.text_muted))
                        .child("No matching fonts"),
                )
            })
            .when(entry_count > 0, |this| {
                this.child(
                    div()
                        .id("settings-font-family-list")
                        .track_scroll(&scroll_handle)
                        .max_h(px(320.0))
                        .py_1()
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .children(rows)
                        .vertical_scrollbar_for(&scroll_handle, window, cx),
                )
            })
    }

    fn font_picker_overlay(
        &self,
        settings: &Settings,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.font_picker.open {
            return None;
        }
        let anchor = self.font_picker.anchor?;
        Some(
            deferred(
                anchored()
                    .position(anchor)
                    .anchor(Anchor::TopLeft)
                    .snap_to_window_with_margin(px(8.0))
                    .child(self.font_picker_menu(settings, theme, window, cx)),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    fn model_selector_control(
        &self,
        target: ProviderDropdownTarget,
        settings: &Settings,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let picker = self.model_picker(target);
        let selected = match target {
            ProviderDropdownTarget::Main => settings.ai.main_model.as_ref(),
            ProviderDropdownTarget::Title => settings.ai.title_model.as_ref(),
        };
        let open = picker.open;
        let model_label = selected
            .map(|model_ref| settings.ai.model_label(model_ref))
            .unwrap_or_else(|| "Select model".to_string());
        let search_input = self.model_search_input(target).clone();

        div()
            .w(px(280.0))
            .h(px(32.0))
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .border_1()
            .border_color(rgb(if open {
                theme.accent
            } else {
                theme.border_variant
            }))
            .bg(rgb(theme.surface_background))
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.main_model_picker.open = false;
                    this.main_model_picker.anchor = None;
                    this.title_model_picker.open = false;
                    this.title_model_picker.anchor = None;
                    let picker = this.model_picker_mut(target);
                    picker.open = true;
                    picker.anchor = Some(point(
                        event.position.x - px(8.0),
                        event.position.y + px(20.0),
                    ));
                    this.model_search_input(target)
                        .read(cx)
                        .focus_handle
                        .clone()
                        .focus(window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .when(!open, |this| {
                        this.child(div().truncate().child(model_label))
                    })
                    .when(open, |this| this.child(search_input)),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(rgb(theme.text_muted))
                    .child(if open { "⌃" } else { "⌄" }),
            )
    }

    fn settings_model_entries(settings: &Settings, query: &str) -> Vec<SettingsModelMenuEntry> {
        let query = query.trim().to_ascii_lowercase();
        let mut entries = Vec::new();
        for provider in AiProvider::ALL {
            let config = settings.ai.provider_config(provider);
            if !config.enabled || config.models.is_empty() {
                continue;
            }
            let provider_matches = provider.label().to_ascii_lowercase().contains(&query);
            let mut model_entries = config
                .models
                .into_iter()
                .filter(|model| {
                    query.is_empty()
                        || provider_matches
                        || model.id.to_ascii_lowercase().contains(&query)
                        || model.name.to_ascii_lowercase().contains(&query)
                        || model.group.to_ascii_lowercase().contains(&query)
                })
                .map(move |model| SettingsModelMenuEntry::Model(provider, model))
                .collect::<Vec<_>>();
            if !model_entries.is_empty() {
                entries.push(SettingsModelMenuEntry::Header(provider));
                entries.append(&mut model_entries);
            }
        }
        entries
    }

    fn model_picker_option(
        target: ProviderDropdownTarget,
        entry: SettingsModelMenuEntry,
        selected: Option<ModelRef>,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match entry {
            SettingsModelMenuEntry::Header(provider) => div()
                .h(px(26.0))
                .px_3()
                .flex()
                .items_center()
                .text_size(ui_rems(11.0))
                .text_color(rgb(theme.text_muted))
                .child(provider.label())
                .into_any_element(),
            SettingsModelMenuEntry::Model(provider, model) => {
                let model_id = model.id.clone();
                let is_selected = selected.as_ref().is_some_and(|model_ref| {
                    model_ref.provider == provider && model_ref.id == model.id
                });
                div()
                    .h(px(30.0))
                    .pl(px(24.0))
                    .pr_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .rounded(px(4.0))
                    .bg(if is_selected {
                        rgb(theme.element_selected)
                    } else {
                        rgba(0x00000000)
                    })
                    .text_color(rgb(if is_selected {
                        theme.accent
                    } else {
                        theme.text
                    }))
                    .hover(move |style| style.bg(rgb(theme.element_hover)))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.select_model(target, provider, model_id.clone(), cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(div().truncate().min_w_0().child(model.name))
                    .child(
                        div()
                            .flex_none()
                            .text_size(ui_rems(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(model.group),
                    )
                    .into_any_element()
            }
        }
    }

    fn model_picker_menu(
        &self,
        target: ProviderDropdownTarget,
        settings: &Settings,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let query = self.model_picker(target).query.to_string();
        let entries = Self::settings_model_entries(settings, &query);
        let entry_count = entries.len();
        let selected = match target {
            ProviderDropdownTarget::Main => settings.ai.main_model.clone(),
            ProviderDropdownTarget::Title => settings.ai.title_model.clone(),
        };
        let scroll_handle = self.model_picker(target).scroll_handle.clone();
        let list_id: ElementId = match target {
            ProviderDropdownTarget::Main => "settings-main-model-list".into(),
            ProviderDropdownTarget::Title => "settings-title-model-list".into(),
        };

        let rows = entries
            .into_iter()
            .map(|entry| Self::model_picker_option(target, entry, selected.clone(), theme, cx))
            .collect::<Vec<_>>();

        div()
            .w(px(280.0))
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .shadow_sm()
            .overflow_hidden()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .when(entry_count == 0, |this| {
                this.child(div().px_3().py_2().text_color(rgb(theme.text_muted)).child(
                    if query.trim().is_empty() {
                        "No enabled provider models"
                    } else {
                        "No matching models"
                    },
                ))
            })
            .when(entry_count > 0, |this| {
                this.child(
                    div()
                        .id(list_id)
                        .track_scroll(&scroll_handle)
                        .max_h(px(320.0))
                        .py_1()
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .children(rows)
                        .vertical_scrollbar_for(&scroll_handle, window, cx),
                )
            })
    }

    fn model_picker_overlay(
        &self,
        target: ProviderDropdownTarget,
        settings: &Settings,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let picker = self.model_picker(target);
        if !picker.open {
            return None;
        }
        let anchor = picker.anchor?;
        Some(
            deferred(
                anchored()
                    .position(anchor)
                    .anchor(Anchor::TopLeft)
                    .snap_to_window_with_margin(px(8.0))
                    .child(self.model_picker_menu(target, settings, theme, window, cx)),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    fn ai_action_button(
        &self,
        label: &'static str,
        theme: Theme,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .h(px(28.0))
            .min_w(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    on_click(this, cx);
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn provider_status(&self, theme: Theme) -> Option<impl IntoElement> {
        self.provider_model_picker
            .status
            .clone()
            .map(move |status| {
                div()
                    .text_size(ui_rems(12.0))
                    .text_color(rgb(theme.text_muted))
                    .child(status)
            })
    }

    fn base_url_default_button(
        &self,
        provider: AiProvider,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .h(px(28.0))
            .min_w(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    let base_url = provider.default_base_url().to_string();
                    this.ai_base_url_input
                        .update(cx, |input, cx| input.set_content(base_url.into(), cx));
                    this.update_provider_config(
                        provider,
                        move |_, config| config.base_url = None,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .child("Default")
    }

    fn section_title(label: &'static str, theme: Theme) -> impl IntoElement {
        div()
            .pt_2()
            .pb_1()
            .text_color(rgb(theme.text_muted))
            .child(label)
    }

    fn keymap_section(
        &self,
        settings: &Settings,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(Self::section_title("Keymap", theme))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .h(px(44.0))
                    .child(div().text_color(rgb(theme.text)).child("Base preset"))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(self.keymap_option(
                                KeymapPreset::Modern,
                                settings.keymap_preset,
                                theme,
                                cx,
                            ))
                            .child(self.keymap_option(
                                KeymapPreset::Vim,
                                settings.keymap_preset,
                                theme,
                                cx,
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .h(px(44.0))
                    .child(div().text_color(rgb(theme.text)).child("Keymap editor"))
                    .child(self.keybinding_button(
                        "Open Keymap",
                        theme,
                        |_, _, cx| crate::open_keymap_in_active_workspace(cx),
                        cx,
                    )),
            )
            .into_any_element()
    }

    fn keymap_tab(
        &self,
        settings: &Settings,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        vec![self.keymap_section(settings, theme, cx)]
    }

    fn general_tab(
        &self,
        settings: &Settings,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        vec![
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title("Zoom", theme))
                .child(self.number_row(
                    "Default zoom",
                    settings.behavior.default_zoom,
                    |settings| settings.behavior.default_zoom -= NUMBER_STEP_SMALL,
                    |settings| settings.behavior.default_zoom += NUMBER_STEP_SMALL,
                    theme,
                    cx,
                ))
                .child(self.number_row(
                    "Zoom step",
                    settings.behavior.zoom_step,
                    |settings| settings.behavior.zoom_step -= NUMBER_STEP_SMALL,
                    |settings| settings.behavior.zoom_step += NUMBER_STEP_SMALL,
                    theme,
                    cx,
                ))
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title("Scroll", theme))
                .child(self.toggle_row(
                    "Smooth scrolling",
                    settings.behavior.smooth_scrolling,
                    |settings| {
                        settings.behavior.smooth_scrolling = !settings.behavior.smooth_scrolling
                    },
                    theme,
                    cx,
                ))
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title("Window", theme))
                .child(self.number_row(
                    "Opacity",
                    settings.window.opacity,
                    |settings| settings.window.opacity -= OPACITY_STEP,
                    |settings| settings.window.opacity += OPACITY_STEP,
                    theme,
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .h(px(44.0))
                        .child(div().text_color(rgb(theme.text)).child("Background"))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(self.background_option(
                                    "Blurred",
                                    WindowBackgroundMode::Blurred,
                                    settings.window.background,
                                    theme,
                                    cx,
                                ))
                                .child(self.background_option(
                                    "Transparent",
                                    WindowBackgroundMode::Transparent,
                                    settings.window.background,
                                    theme,
                                    cx,
                                ))
                                .child(self.background_option(
                                    "Opaque",
                                    WindowBackgroundMode::Opaque,
                                    settings.window.background,
                                    theme,
                                    cx,
                                )),
                        ),
                )
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title("Tabs", theme))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .h(px(44.0))
                        .child(div().text_color(rgb(theme.text)).child("Tab bar"))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(self.tab_bar_visibility_option(
                                    "Auto",
                                    TabBarVisibility::Auto,
                                    settings.tab_bar_visibility,
                                    theme,
                                    cx,
                                ))
                                .child(self.tab_bar_visibility_option(
                                    "Always",
                                    TabBarVisibility::Always,
                                    settings.tab_bar_visibility,
                                    theme,
                                    cx,
                                )),
                        ),
                )
                .child(self.toggle_row(
                    "Dim inactive panes",
                    settings.behavior.dim_inactive_panes,
                    |settings| {
                        settings.behavior.dim_inactive_panes = !settings.behavior.dim_inactive_panes
                    },
                    theme,
                    cx,
                ))
                .into_any_element(),
        ]
    }

    fn ai_general_tab(
        &self,
        settings: &Settings,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let input_row = |label: &'static str, child: AnyElement| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .min_h(px(48.0))
                .gap_3()
                .child(
                    div()
                        .flex_none()
                        .w(px(104.0))
                        .text_color(rgb(theme.text))
                        .child(label),
                )
                .child(child)
        };

        vec![
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(Self::section_title("Default models", theme))
                .child(input_row(
                    "Main model",
                    self.model_selector_control(ProviderDropdownTarget::Main, settings, theme, cx)
                        .into_any_element(),
                ))
                .child(input_row(
                    "Title model",
                    self.model_selector_control(ProviderDropdownTarget::Title, settings, theme, cx)
                        .into_any_element(),
                ))
                .child(
                    div()
                        .text_size(ui_rems(12.0))
                        .text_color(rgb(theme.text_muted))
                        .child("Only models from enabled providers' model lists appear here."),
                )
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title("Quick AI chat", theme))
                .child(self.number_row(
                    "Transparency",
                    settings.ai.quick_ask.transparency,
                    |settings| settings.ai.quick_ask.transparency -= OPACITY_STEP,
                    |settings| settings.ai.quick_ask.transparency += OPACITY_STEP,
                    theme,
                    cx,
                ))
                .child(self.number_row(
                    "Text size",
                    settings.ai.quick_ask.text_size,
                    |settings| settings.ai.quick_ask.text_size -= FONT_SIZE_STEP,
                    |settings| settings.ai.quick_ask.text_size += FONT_SIZE_STEP,
                    theme,
                    cx,
                ))
                .into_any_element(),
        ]
    }

    fn ai_providers_tab(
        &self,
        settings: &Settings,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let provider = self.selected_ai_provider;
        let config = settings.ai.provider_config(provider);
        let provider_entries = AiProvider::ALL
            .into_iter()
            .map(|provider| {
                self.provider_sidebar_entry(
                    provider,
                    settings.ai.provider_config(provider).enabled,
                    theme,
                    cx,
                )
            })
            .collect::<Vec<_>>();
        let model_rows = config
            .models
            .iter()
            .cloned()
            .map(|model| self.provider_model_row(provider, model, theme, cx))
            .collect::<Vec<_>>();

        let input_row = |label: &'static str, child: AnyElement| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .min_h(px(44.0))
                .gap_3()
                .child(
                    div()
                        .flex_none()
                        .w(px(88.0))
                        .text_color(rgb(theme.text))
                        .child(label),
                )
                .child(child)
        };

        vec![
            div()
                .flex()
                .flex_row()
                .min_h(px(520.0))
                .border_1()
                .border_color(rgb(theme.border_variant))
                .bg(rgb(theme.surface_background))
                .child(
                    div()
                        .w(px(190.0))
                        .flex_none()
                        .flex()
                        .flex_col()
                        .border_r(px(1.0))
                        .border_color(rgb(theme.border_variant))
                        .children(provider_entries),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .p_4()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .child(
                                            div()
                                                .text_size(ui_rems(16.0))
                                                .text_color(rgb(theme.text))
                                                .child(provider.label()),
                                        )
                                        .child(
                                            div()
                                                .text_size(ui_rems(12.0))
                                                .text_color(rgb(theme.text_muted))
                                                .child(provider.default_base_url()),
                                        ),
                                )
                                .child(self.toggle_row(
                                    "Enabled",
                                    config.enabled,
                                    move |settings| {
                                        let config = settings.ai.provider_config_mut(provider);
                                        config.enabled = !config.enabled;
                                    },
                                    theme,
                                    cx,
                                )),
                        )
                        .child(input_row(
                            "API key",
                            self.ai_key_input.clone().into_any_element(),
                        ))
                        .child(input_row(
                            "Base URL",
                            div()
                                .flex()
                                .flex_row()
                                .flex_1()
                                .min_w_0()
                                .gap_2()
                                .child(self.ai_base_url_input.clone())
                                .child(self.base_url_default_button(provider, theme, cx))
                                .into_any_element(),
                        ))
                        .when_some(self.provider_status(theme), |this, status| {
                            this.child(div().flex().justify_end().child(status))
                        })
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(self.ai_action_button(
                                    "Test connection",
                                    theme,
                                    move |this, cx| this.test_connection(provider, cx),
                                    cx,
                                ))
                                .child(self.fetch_models_button(provider, theme, cx)),
                        )
                        .child(Self::section_title("Model list", theme))
                        .child(div().flex().flex_col().gap_1().children(model_rows).when(
                            config.models.is_empty(),
                            |this| {
                                this.child(
                                    div()
                                        .py_2()
                                        .text_color(rgb(theme.text_muted))
                                        .child("No models added yet."),
                                )
                            },
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .child(self.ai_model_input.clone())
                                .child(self.add_manual_model_button(provider, theme, cx)),
                        ),
                )
                .into_any_element(),
        ]
    }

    fn provider_sidebar_entry(
        &self,
        provider: AiProvider,
        enabled: bool,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selected_ai_provider == provider;
        div()
            .h(px(34.0))
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .text_color(rgb(if selected {
                theme.text
            } else {
                theme.text_muted
            }))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.select_ai_provider(provider, cx);
                    cx.stop_propagation();
                }),
            )
            .child(div().truncate().child(provider.label()))
            .child(
                div()
                    .text_size(ui_rems(11.0))
                    .text_color(rgb(if enabled {
                        theme.success
                    } else {
                        theme.text_muted
                    }))
                    .child(if enabled { "On" } else { "Off" }),
            )
            .into_any_element()
    }

    fn provider_model_row(
        &self,
        provider: AiProvider,
        model: AiModel,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let model_id = model.id.clone();
        div()
            .min_h(px(42.0))
            .px_2()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.panel_background))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(div().truncate().child(model.name))
                    .child(
                        div()
                            .truncate()
                            .text_size(ui_rems(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(model.id),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(ui_rems(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(model.group),
                    )
                    .child(self.remove_model_button(provider, model_id, theme, cx)),
            )
            .into_any_element()
    }

    fn remove_model_button(
        &self,
        provider: AiProvider,
        model_id: String,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .h(px(24.0))
            .px_2()
            .flex()
            .items_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    let mut settings = cx.global::<SettingsStore>().settings.clone();
                    settings
                        .ai
                        .provider_config_mut(provider)
                        .remove_model(&model_id);
                    if settings.ai.main_model.as_ref().is_some_and(|model_ref| {
                        model_ref.provider == provider && model_ref.id == model_id
                    }) {
                        settings.ai.main_model = None;
                    }
                    if settings.ai.title_model.as_ref().is_some_and(|model_ref| {
                        model_ref.provider == provider && model_ref.id == model_id
                    }) {
                        settings.ai.title_model = None;
                    }
                    this.persist_settings(settings, cx);
                    cx.stop_propagation();
                }),
            )
            .child("Remove")
    }

    fn add_manual_model_button(
        &self,
        provider: AiProvider,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .h(px(28.0))
            .px_2()
            .flex()
            .items_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    let model_id = this.ai_model_input.read(cx).content.trim().to_string();
                    if model_id.is_empty() {
                        cx.stop_propagation();
                        return;
                    }
                    let mut settings = cx.global::<SettingsStore>().settings.clone();
                    settings
                        .ai
                        .provider_config_mut(provider)
                        .ensure_model(AiModel::new(model_id.clone(), model_id, provider.label()));
                    this.persist_settings(settings, cx);
                    this.ai_model_input
                        .update(cx, |input, cx| input.set_content("".into(), cx));
                    cx.stop_propagation();
                }),
            )
            .child("Add")
    }

    fn fetch_models_button(
        &self,
        provider: AiProvider,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let loading = self.provider_model_picker.loading;
        div()
            .h(px(28.0))
            .px_2()
            .flex()
            .items_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let anchor = point(event.position.x - px(420.0), event.position.y + px(18.0));
                    this.fetch_provider_models(provider, Some(anchor), cx);
                    cx.stop_propagation();
                }),
            )
            .child(if loading {
                "Fetching…"
            } else {
                "Fetch models"
            })
    }

    fn toggle_fetched_model(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(index) = self
            .provider_model_selection
            .iter()
            .position(|selected| selected == &id)
        {
            self.provider_model_selection.remove(index);
        } else {
            self.provider_model_selection.push(id);
        }
        cx.notify();
    }

    fn confirm_fetched_models(&mut self, provider: AiProvider, cx: &mut Context<Self>) {
        let selected = self.provider_model_selection.clone();
        if selected.is_empty() {
            self.provider_model_picker.open = false;
            self.provider_model_picker.anchor = None;
            cx.notify();
            return;
        }

        let fetched = self.provider_model_picker.models.clone();
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        let config = settings.ai.provider_config_mut(provider);
        for model in fetched {
            if !selected.iter().any(|id| id == &model.id) {
                continue;
            }
            let group = model
                .owned_by
                .unwrap_or_else(|| provider.label().to_string());
            config.ensure_model(AiModel::new(model.id, model.label, group));
        }
        self.persist_settings(settings, cx);
        self.provider_model_picker.open = false;
        self.provider_model_picker.anchor = None;
        self.provider_model_selection.clear();
        cx.notify();
    }

    fn provider_model_fetch_overlay(
        &self,
        provider: AiProvider,
        settings: &Settings,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.provider_model_picker.open {
            return None;
        }
        let anchor = self.provider_model_picker.anchor?;
        let query = self.provider_model_picker.query.trim().to_ascii_lowercase();
        let models = self
            .provider_model_picker
            .models
            .iter()
            .filter(|model| {
                query.is_empty()
                    || model.id.to_ascii_lowercase().contains(&query)
                    || model.label.to_ascii_lowercase().contains(&query)
            })
            .cloned()
            .collect::<Vec<_>>();
        let total_count = self.provider_model_picker.models.len();
        let selected = self.provider_model_selection.clone();
        let existing = settings.ai.provider_config(provider).models;
        let entry_count = models.len();
        let scroll_handle = self.provider_model_picker.fetch_scroll_handle.clone();
        let search_input = self.fetch_model_search.clone();

        Some(
            deferred(
                anchored()
                    .position(anchor)
                    .anchor(Anchor::TopLeft)
                    .snap_to_window_with_margin(px(8.0))
                    .child(
                        div()
                            .w(px(440.0))
                            .flex()
                            .flex_col()
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(rgb(theme.border_variant))
                            .bg(rgb(theme.surface_background))
                            .shadow_sm()
                            .overflow_hidden()
                            .occlude()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .border_b(px(1.0))
                                    .border_color(rgb(theme.border_variant))
                                    .text_color(rgb(theme.text))
                                    .child("Choose models to add"),
                            )
                            .child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .border_b(px(1.0))
                                    .border_color(rgb(theme.border_variant))
                                    .flex()
                                    .child(search_input),
                            )
                            .when(entry_count == 0, |this| {
                                this.child(
                                    div().px_3().py_2().text_color(rgb(theme.text_muted)).child(
                                        if total_count == 0 {
                                            "No models returned."
                                        } else {
                                            "No matching models."
                                        },
                                    ),
                                )
                            })
                            .when(entry_count > 0, |this| {
                                let list_height = 42.0 * entry_count.min(9) as f32;
                                this.child(
                                    div()
                                        .h(px(list_height))
                                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                                        .child(
                                            uniform_list(
                                                "settings-provider-fetched-models",
                                                entry_count,
                                                cx.processor(
                                                    move |_, visible_range: Range<usize>, _, cx| {
                                                        visible_range
                                                            .map(|ix| {
                                                                let model = models[ix].clone();
                                                                let already_added =
                                                                    existing.iter().any(|entry| {
                                                                        entry.id == model.id
                                                                    });
                                                                let checked = selected
                                                                    .iter()
                                                                    .any(|id| id == &model.id);
                                                                Self::fetched_model_row(
                                                                    provider,
                                                                    model,
                                                                    already_added,
                                                                    checked,
                                                                    theme,
                                                                    cx,
                                                                )
                                                            })
                                                            .collect::<Vec<_>>()
                                                    },
                                                ),
                                            )
                                            .track_scroll(&scroll_handle)
                                            .with_decoration(uniform_list_scrollbar_for(
                                                &scroll_handle,
                                                "settings-provider-fetched-models",
                                                window,
                                                cx,
                                            ))
                                            .size_full(),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .border_t(px(1.0))
                                    .border_color(rgb(theme.border_variant))
                                    .flex()
                                    .justify_end()
                                    .gap_2()
                                    .child(self.ai_action_button(
                                        "Cancel",
                                        theme,
                                        |this, cx| {
                                            this.provider_model_picker.open = false;
                                            this.provider_model_picker.anchor = None;
                                            this.provider_model_selection.clear();
                                            cx.notify();
                                        },
                                        cx,
                                    ))
                                    .child(self.ai_action_button(
                                        "Add selected",
                                        theme,
                                        move |this, cx| this.confirm_fetched_models(provider, cx),
                                        cx,
                                    )),
                            ),
                    ),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }

    fn fetched_model_row(
        provider: AiProvider,
        model: AiModelInfo,
        already_added: bool,
        checked: bool,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = model.id.clone();
        let meta = model
            .owned_by
            .unwrap_or_else(|| provider.label().to_string());
        div()
            .h(px(42.0))
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .text_color(rgb(if already_added {
                theme.text_muted
            } else {
                theme.text
            }))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    if !already_added {
                        this.toggle_fetched_model(id.clone(), cx);
                    }
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .w(px(28.0))
                    .text_color(rgb(if checked {
                        theme.accent
                    } else {
                        theme.text_muted
                    }))
                    .child(if already_added {
                        "✓"
                    } else if checked {
                        "☑"
                    } else {
                        "☐"
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().child(model.label))
                    .child(
                        div()
                            .truncate()
                            .text_size(ui_rems(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(model.id),
                    ),
            )
            .child(
                div()
                    .max_w(px(96.0))
                    .truncate()
                    .text_size(ui_rems(11.0))
                    .text_color(rgb(theme.text_muted))
                    .child(meta),
            )
            .into_any_element()
    }

    fn appearance_tab(
        &self,
        settings: &Settings,
        resolved: Theme,
        themes: Vec<ThemeMetadata>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let page_gap = resolved.page_gap;
        let theme_options = themes
            .into_iter()
            .map(|metadata| {
                self.theme_option(metadata, &settings.theme_name, resolved, cx)
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        vec![
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title("Theme", resolved))
                .child(div().flex().flex_col().gap_2().children(theme_options))
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title("Fonts", resolved))
                .child(self.font_family_row(settings, resolved, cx))
                .child(self.text_input_row(
                    "UI font fallbacks",
                    self.ui_font_fallbacks_input.clone(),
                    resolved,
                ))
                .child(
                    div()
                        .text_size(ui_rems(12.0))
                        .text_color(rgb(resolved.text_muted))
                        .child("Separate fallback font names with commas."),
                )
                .child(self.number_row(
                    "UI font size",
                    settings.font.ui_font_size,
                    |settings| settings.font.ui_font_size -= FONT_SIZE_STEP,
                    |settings| settings.font.ui_font_size += FONT_SIZE_STEP,
                    resolved,
                    cx,
                ))
                .child(self.number_row(
                    "UI font weight",
                    settings.font.ui_font_weight,
                    |settings| settings.font.ui_font_weight -= FONT_WEIGHT_STEP,
                    |settings| settings.font.ui_font_weight += FONT_WEIGHT_STEP,
                    resolved,
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .child(self.reset_font_settings_button(resolved, cx)),
                )
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title("Overrides", resolved))
                .child(self.number_row(
                    "Page gap",
                    page_gap,
                    move |settings| settings.overrides.page_gap = Some(page_gap - PAGE_GAP_STEP),
                    move |settings| settings.overrides.page_gap = Some(page_gap + PAGE_GAP_STEP),
                    resolved,
                    cx,
                ))
                .when(settings.overrides.page_gap.is_some(), |this| {
                    this.child(div().flex().justify_end().child(self.small_button(
                        "Reset page gap",
                        |settings| settings.overrides.page_gap = None,
                        resolved,
                        cx,
                    )))
                })
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Background",
                        current: resolved.background,
                        override_value: settings.overrides.background,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.background = Some(color),
                    |overrides| overrides.background = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Page",
                        current: resolved.page,
                        override_value: settings.overrides.page,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.page = Some(color),
                    |overrides| overrides.page = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Selection highlight",
                        current: resolved.selection_highlight,
                        override_value: settings.overrides.selection_highlight,
                        swatches: &HIGHLIGHT_SWATCHES,
                        with_alpha: true,
                    },
                    |overrides, color| overrides.selection_highlight = Some(color),
                    |overrides| overrides.selection_highlight = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Footer background",
                        current: resolved.footer_background,
                        override_value: settings.overrides.footer_background,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.footer_background = Some(color),
                    |overrides| overrides.footer_background = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Footer border",
                        current: resolved.footer_border,
                        override_value: settings.overrides.footer_border,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.footer_border = Some(color),
                    |overrides| overrides.footer_border = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Footer text",
                        current: resolved.footer_text,
                        override_value: settings.overrides.footer_text,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.footer_text = Some(color),
                    |overrides| overrides.footer_text = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Panel background",
                        current: resolved.panel_background,
                        override_value: settings.overrides.panel_background,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.panel_background = Some(color),
                    |overrides| overrides.panel_background = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Panel border",
                        current: resolved.panel_border,
                        override_value: settings.overrides.panel_border,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.panel_border = Some(color),
                    |overrides| overrides.panel_border = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Surface background",
                        current: resolved.surface_background,
                        override_value: settings.overrides.surface_background,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.surface_background = Some(color),
                    |overrides| overrides.surface_background = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Border",
                        current: resolved.border,
                        override_value: settings.overrides.border,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.border = Some(color),
                    |overrides| overrides.border = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Border variant",
                        current: resolved.border_variant,
                        override_value: settings.overrides.border_variant,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.border_variant = Some(color),
                    |overrides| overrides.border_variant = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Border focused",
                        current: resolved.border_focused,
                        override_value: settings.overrides.border_focused,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.border_focused = Some(color),
                    |overrides| overrides.border_focused = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Tab bar background",
                        current: resolved.tab_bar_background,
                        override_value: settings.overrides.tab_bar_background,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.tab_bar_background = Some(color),
                    |overrides| overrides.tab_bar_background = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Tab active background",
                        current: resolved.tab_active_background,
                        override_value: settings.overrides.tab_active_background,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.tab_active_background = Some(color),
                    |overrides| overrides.tab_active_background = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Tab inactive background",
                        current: resolved.tab_inactive_background,
                        override_value: settings.overrides.tab_inactive_background,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.tab_inactive_background = Some(color),
                    |overrides| overrides.tab_inactive_background = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Element hover",
                        current: resolved.element_hover,
                        override_value: settings.overrides.element_hover,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.element_hover = Some(color),
                    |overrides| overrides.element_hover = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Element selected",
                        current: resolved.element_selected,
                        override_value: settings.overrides.element_selected,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.element_selected = Some(color),
                    |overrides| overrides.element_selected = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Text",
                        current: resolved.text,
                        override_value: settings.overrides.text,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.text = Some(color),
                    |overrides| overrides.text = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Text muted",
                        current: resolved.text_muted,
                        override_value: settings.overrides.text_muted,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.text_muted = Some(color),
                    |overrides| overrides.text_muted = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Placeholder text",
                        current: resolved.placeholder_text,
                        override_value: settings.overrides.placeholder_text,
                        swatches: &ALPHA_SWATCHES,
                        with_alpha: true,
                    },
                    |overrides, color| overrides.placeholder_text = Some(color),
                    |overrides| overrides.placeholder_text = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Accent",
                        current: resolved.accent,
                        override_value: settings.overrides.accent,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.accent = Some(color),
                    |overrides| overrides.accent = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Match highlight",
                        current: resolved.match_highlight,
                        override_value: settings.overrides.match_highlight,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.match_highlight = Some(color),
                    |overrides| overrides.match_highlight = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Selected row",
                        current: resolved.selected_row,
                        override_value: settings.overrides.selected_row,
                        swatches: &ALPHA_SWATCHES,
                        with_alpha: true,
                    },
                    |overrides, color| overrides.selected_row = Some(color),
                    |overrides| overrides.selected_row = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Overlay",
                        current: resolved.overlay,
                        override_value: settings.overrides.overlay,
                        swatches: &ALPHA_SWATCHES,
                        with_alpha: true,
                    },
                    |overrides, color| overrides.overlay = Some(color),
                    |overrides| overrides.overlay = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Control background",
                        current: resolved.control_background,
                        override_value: settings.overrides.control_background,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.control_background = Some(color),
                    |overrides| overrides.control_background = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Control border",
                        current: resolved.control_border,
                        override_value: settings.overrides.control_border,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.control_border = Some(color),
                    |overrides| overrides.control_border = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Control hover",
                        current: resolved.control_hover,
                        override_value: settings.overrides.control_hover,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.control_hover = Some(color),
                    |overrides| overrides.control_hover = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Success",
                        current: resolved.success,
                        override_value: settings.overrides.success,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.success = Some(color),
                    |overrides| overrides.success = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Error",
                        current: resolved.error,
                        override_value: settings.overrides.error,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.error = Some(color),
                    |overrides| overrides.error = None,
                    resolved,
                    cx,
                ))
                .into_any_element(),
        ]
    }

    fn export_button(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(28.0))
            .min_w(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.export(window, cx);
                    cx.stop_propagation();
                }),
            )
            .child("Export")
    }
}

impl EventEmitter<PanelEvent> for SettingsWindow {}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui_font = setup_ui_font(window, cx);
        let (settings, resolved, themes) = {
            let store = cx.global::<SettingsStore>();
            (
                store.settings.clone(),
                store.resolved_theme,
                store.registry.metadata(),
            )
        };
        let content = match self.active_tab {
            SettingsTab::General => self.general_tab(&settings, resolved, cx),
            SettingsTab::Keymap => self.keymap_tab(&settings, resolved, cx),
            SettingsTab::Appearance => self.appearance_tab(&settings, resolved, themes, cx),
            SettingsTab::AiGeneral => self.ai_general_tab(&settings, resolved, cx),
            SettingsTab::AiProviders => self.ai_providers_tab(&settings, resolved, cx),
        };
        let (main_model_overlay, title_model_overlay) = if self.active_tab == SettingsTab::AiGeneral
        {
            (
                self.model_picker_overlay(
                    ProviderDropdownTarget::Main,
                    &settings,
                    resolved,
                    window,
                    cx,
                ),
                self.model_picker_overlay(
                    ProviderDropdownTarget::Title,
                    &settings,
                    resolved,
                    window,
                    cx,
                ),
            )
        } else {
            (None, None)
        };
        let provider_model_overlay = if self.active_tab == SettingsTab::AiProviders {
            self.provider_model_fetch_overlay(
                self.selected_ai_provider,
                &settings,
                resolved,
                window,
                cx,
            )
        } else {
            None
        };
        let font_picker_overlay = if self.active_tab == SettingsTab::Appearance {
            self.font_picker_overlay(&settings, resolved, window, cx)
        } else {
            None
        };

        div()
            .key_context("SettingsPanel")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(rgb(resolved.surface_background))
            .font(ui_font)
            .text_color(rgb(resolved.text))
            .text_size(ui_rems(13.0))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    let mut changed = false;
                    if this.main_model_picker.open {
                        this.main_model_picker.open = false;
                        this.main_model_picker.anchor = None;
                        changed = true;
                    }
                    if this.title_model_picker.open {
                        this.title_model_picker.open = false;
                        this.title_model_picker.anchor = None;
                        changed = true;
                    }
                    if this.font_picker.open {
                        this.font_picker.open = false;
                        this.font_picker.anchor = None;
                        changed = true;
                    }
                    this.focus_handle.clone().focus(window, cx);
                    if changed {
                        cx.notify();
                    }
                    cx.stop_propagation();
                }),
            )
            .on_action(cx.listener(Self::close))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .flex_none()
                    .h(px(50.0))
                    .px_4()
                    .border_b(px(1.0))
                    .border_color(rgb(resolved.border))
                    .child(div().text_size(ui_rems(16.0)).child("Settings"))
                    .child(div().text_color(rgb(resolved.text_muted)).child("Esc")),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(self.settings_nav(resolved, cx))
                    .child(
                        div()
                            .id("settings-content")
                            .track_scroll(&self.content_scroll)
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .min_w_0()
                            .overflow_y_scroll()
                            .px_4()
                            .py_3()
                            .children(content)
                            .vertical_scrollbar_for(&self.content_scroll, window, cx),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_t(px(1.0))
                    .border_color(rgb(resolved.border))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_end()
                            .child(self.export_button(resolved, cx)),
                    )
                    .when_some(self.status.clone(), |this, status| {
                        this.child(div().text_color(rgb(resolved.success)).child(status))
                    })
                    .when_some(self.save_error.clone(), |this, error| {
                        this.child(div().text_color(rgb(resolved.error)).child(error))
                    }),
            )
            .when_some(main_model_overlay, |this, overlay| this.child(overlay))
            .when_some(title_model_overlay, |this, overlay| this.child(overlay))
            .when_some(provider_model_overlay, |this, overlay| this.child(overlay))
            .when_some(font_picker_overlay, |this, overlay| this.child(overlay))
    }
}
