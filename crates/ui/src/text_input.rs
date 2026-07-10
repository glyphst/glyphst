//! Reusable single-line text input rendering with overflow clipping and
//! scroll-to-cursor, mirroring how Zed's `Editor` keeps the caret visible.
//!
//! Zed clips painted text to the viewport with `Window::with_content_mask`
//! (`crates/gpui/src/window.rs`) and scrolls horizontally so the cursor stays
//! in view via `Editor::autoscroll_horizontally`
//! (`crates/editor/src/scroll/autoscroll.rs`). This module adapts both ideas to
//! a single-line input so that text wider than the field is hidden instead of
//! overflowing the border.

use std::ops::Range;

use gpui::{
    App, Bounds, ContentMask, Context, DispatchPhase, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, Focusable, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, MouseButton, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ShapedLine,
    SharedString, Style, TextRun, Window, fill, point, px, relative, rgb, rgba, size,
};

use crate::settings::SettingsStore;

/// Glyph used to mask sensitive input (e.g. API keys).
pub const MASK_GLYPH: &str = "•";

/// State each single-line input entity must expose so the shared
/// [`SingleLineTextElement`] can render it.
///
/// `display_offset` / `display_range` default to the identity, which suits
/// non-masked inputs. Masked inputs (see `SettingsTextInput`) override them to
/// translate content offsets into masked-display offsets.
pub trait SingleLineInput: EntityInputHandler + Focusable + 'static {
    fn content(&self) -> &SharedString;
    fn placeholder(&self) -> SharedString;
    fn mask(&self) -> bool;
    fn selected_range(&self) -> &Range<usize>;
    fn marked_range(&self) -> Option<&Range<usize>>;
    fn cursor_offset(&self) -> usize;
    fn scroll_offset(&self) -> Pixels;

    /// Map a content offset to a display-text offset. Identity by default.
    fn display_offset(&self, offset: usize) -> usize {
        offset.min(self.content().len())
    }

    /// Map a content range to a display-text range. Identity by default.
    fn display_range(&self, range: &Range<usize>) -> Range<usize> {
        range.clone()
    }

    /// Persist the shaped line, element bounds and scroll offset after paint.
    fn set_render_state(&mut self, layout: ShapedLine, bounds: Bounds<Pixels>, scroll: Pixels);

    /// Persist a scroll offset computed from mouse-drag autoscroll.
    fn set_scroll_offset(&mut self, scroll: Pixels);

    /// Whether a mouse drag (selection) is currently in progress.
    ///
    /// Used by the shared element to drive per-frame drag-autoscroll past the
    /// edges, mirroring Zed's editor which scrolls every frame while a
    /// pending selection/drag targets content outside the viewport.
    fn is_dragging(&self) -> bool;

    /// Map a display-text offset back to a content offset. Identity by
    /// default; masked inputs override this (see `SettingsTextInput`).
    fn content_offset_for_display_offset(&self, offset: usize) -> usize {
        offset.min(self.content().len())
    }

    /// Extend the active selection to `offset` and request a re-render.
    ///
    /// Each entity delegates to its existing `select_to`.
    fn extend_selection_to(&mut self, offset: usize, cx: &mut Context<Self>);

    /// End an active mouse drag, if any.
    fn end_drag(&mut self);
}

/// Zed-style horizontal autoscroll for a single line.
///
/// Returns the scroll offset that keeps `cursor_x` inside the
/// `[scroll, scroll + viewport_width]` viewport, clamped so the text cannot
/// scroll past its end. When the text fits the viewport the offset is zero.
fn autoscroll_to_cursor(
    cursor_x: Pixels,
    viewport_width: Pixels,
    scroll: Pixels,
    text_width: Pixels,
) -> Pixels {
    let max_scroll = (text_width - viewport_width).max(Pixels::ZERO);
    if max_scroll == Pixels::ZERO {
        return Pixels::ZERO;
    }
    let scroll = scroll.min(max_scroll);
    let scroll_left = scroll;
    let scroll_right = scroll_left + viewport_width;
    if cursor_x < scroll_left {
        cursor_x.min(max_scroll)
    } else if cursor_x > scroll_right {
        (cursor_x - viewport_width)
            .min(max_scroll)
            .max(Pixels::ZERO)
    } else {
        scroll
    }
}

const HORIZONTAL_SCROLL_MARGIN_EMS: f32 = 5.0;

fn scale_horizontal_mouse_autoscroll_delta(delta: Pixels) -> f32 {
    (delta.pow(1.2) / 300.0).into()
}

fn horizontal_mouse_autoscroll_delta(
    mouse_x: Pixels,
    bounds: Bounds<Pixels>,
    em_width: Pixels,
) -> Pixels {
    let scroll_space = HORIZONTAL_SCROLL_MARGIN_EMS * em_width;
    let left = bounds.left() + scroll_space;
    let right = bounds.right() - scroll_space;
    let mut scroll_delta = 0.0;

    if mouse_x < left {
        scroll_delta = -scale_horizontal_mouse_autoscroll_delta(left - mouse_x);
    }
    if mouse_x > right {
        scroll_delta = scale_horizontal_mouse_autoscroll_delta(mouse_x - right);
    }

    px(scroll_delta * em_width.as_f32())
}

/// A generic single-line text element. Replaces the per-input
/// `SettingsInputElement` / `SearchTextElement` / `PickerTextElement`.
pub struct SingleLineTextElement<T: SingleLineInput> {
    input: Entity<T>,
}

impl<T: SingleLineInput> SingleLineTextElement<T> {
    pub fn new(input: Entity<T>) -> Self {
        Self { input }
    }
}

impl<T: SingleLineInput> IntoElement for SingleLineTextElement<T> {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

pub struct SingleLineInputPrepaint {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    scroll_offset: Pixels,
}

impl<T: SingleLineInput> Element for SingleLineTextElement<T> {
    type RequestLayoutState = ();
    type PrepaintState = SingleLineInputPrepaint;

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
    ) -> SingleLineInputPrepaint {
        let input = self.input.read(cx);
        let theme = cx.global::<SettingsStore>().resolved_theme;
        let style = window.text_style();
        let focus_handle = input.focus_handle(cx);
        let focused = focus_handle.is_focused(window);

        let display_text: SharedString = if input.mask() && !input.content().is_empty() {
            MASK_GLYPH.repeat(input.content().chars().count()).into()
        } else {
            input.content().clone()
        };

        let (display_text, text_color) = if display_text.is_empty() {
            (input.placeholder(), rgba(theme.placeholder_text).into())
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
        let runs = if !input.content().is_empty() {
            if let Some(marked_range) = input.marked_range().as_ref() {
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

        let dragging = input.is_dragging();
        let scroll_offset = if dragging {
            input.scroll_offset()
        } else if focused {
            let display_cursor = input.display_offset(input.cursor_offset());
            let cursor_pos = line.x_for_index(display_cursor);
            autoscroll_to_cursor(
                cursor_pos,
                bounds.size.width,
                input.scroll_offset(),
                line.width(),
            )
        } else {
            Pixels::ZERO
        };

        // Build the cursor/selection quads using the scroll offset computed above.
        let (selection, cursor_quad) = {
            let input = self.input.read(cx);
            let selected_range = input.selected_range();
            let display_selected_range = input.display_range(selected_range);

            if selected_range.is_empty() {
                let display_cursor = input.display_offset(input.cursor_offset());
                let cursor_pos = line.x_for_index(display_cursor);
                let quad = fill(
                    Bounds::new(
                        point(bounds.left() + cursor_pos - scroll_offset, bounds.top()),
                        size(px(2.0), bounds.bottom() - bounds.top()),
                    ),
                    rgb(theme.accent),
                );
                (None, Some(quad))
            } else {
                let quad = fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(display_selected_range.start)
                                - scroll_offset,
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(display_selected_range.end)
                                - scroll_offset,
                            bounds.bottom(),
                        ),
                    ),
                    rgba(theme.selection_highlight),
                );
                (Some(quad), None)
            }
        };

        SingleLineInputPrepaint {
            line: Some(line),
            cursor: cursor_quad,
            selection,
            scroll_offset,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut SingleLineInputPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle(cx);
        let focused = focus_handle.is_focused(window);
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );

        let scroll = prepaint.scroll_offset;
        let line_origin = point(bounds.origin.x - scroll, bounds.origin.y);
        let input = self.input.clone();
        let line_for_mouse = prepaint.line.as_ref().cloned();

        if let Some(line) = line_for_mouse {
            let style = window.text_style();
            let font_size = style.font_size.to_pixels(window.rem_size());
            let font_id = window.text_system().resolve_font(&style.font());
            let em_width = window
                .text_system()
                .em_width(font_id, font_size)
                .unwrap_or(font_size);
            let input = self.input.clone();

            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
                if phase != DispatchPhase::Bubble || !event.dragging() {
                    return;
                }

                let max_scroll = (line.width() - bounds.size.width).max(Pixels::ZERO);
                input.update(cx, |state, cx| {
                    if !state.is_dragging() {
                        return;
                    }

                    let scroll = state.scroll_offset();
                    let display_offset =
                        index_for_mouse_position_with_scroll(&line, bounds, event.position, scroll);
                    let content_offset = state.content_offset_for_display_offset(display_offset);
                    state.extend_selection_to(content_offset, cx);

                    let scroll_delta =
                        horizontal_mouse_autoscroll_delta(event.position.x, bounds, em_width);
                    let scroll = (scroll + scroll_delta).clamp(Pixels::ZERO, max_scroll);
                    state.set_scroll_offset(scroll);
                });
            });
        }

        let input_for_mouse_up = self.input.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                input_for_mouse_up.update(cx, |state, _| state.end_drag());
            }
        });

        // Clip the selection, text and cursor to the field bounds so overflow
        // is hidden (Zed uses the same content-mask mechanism in its editor).
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(sel) = prepaint.selection.take() {
                window.paint_quad(sel);
            }

            if let Some(line) = prepaint.line.take() {
                if let Err(e) = line.paint(
                    line_origin,
                    window.line_height(),
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                ) {
                    eprintln!("text input paint error: {e:#}");
                }

                if focused && let Some(cursor) = prepaint.cursor.take() {
                    window.paint_quad(cursor);
                }

                input.update(cx, |state, _| {
                    state.set_render_state(line, bounds, scroll);
                });
            }
        });
    }
}

/// Hit-test helper for single-line inputs that scroll horizontally.
///
/// Returns the display-text index for a mouse `position`, accounting for the
/// current `scroll` offset. Callers that mask their content should translate
/// the returned display index back to a content offset.
pub fn index_for_mouse_position_with_scroll(
    line: &ShapedLine,
    bounds: Bounds<Pixels>,
    position: Point<Pixels>,
    scroll: Pixels,
) -> usize {
    line.index_for_x(position.x - bounds.left() + scroll)
        .unwrap_or_else(|| line.len())
}
