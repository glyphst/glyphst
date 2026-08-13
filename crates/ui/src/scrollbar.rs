use std::{cell::RefCell, panic::Location, rc::Rc};

use gpui::{App, Bounds, Div, ElementId, Pixels, Point, Stateful, Window, div, prelude::*, size};

pub use gpui_component::scroll::{
    ScrollableElement, Scrollbar, ScrollbarAxis, ScrollbarHandle, ScrollbarMode,
};

/// Compatibility builder for Glyphst surfaces that already own their scroll
/// area and handle. Attach it to the stationary viewport host, with the
/// scrolling element as a sibling of the rendered gpui-component [`Scrollbar`].
pub trait WithScrollbar: Sized {
    type Output;

    #[track_caller]
    fn vertical_scrollbar_for<H: ScrollbarHandle + Clone>(
        self,
        scroll_handle: &H,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::Output;

    fn vertical_scrollbar_for_with_id<H: ScrollbarHandle + Clone>(
        self,
        scroll_handle: &H,
        id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::Output;
}

impl WithScrollbar for Div {
    type Output = Self;

    #[track_caller]
    fn vertical_scrollbar_for<H: ScrollbarHandle + Clone>(
        self,
        scroll_handle: &H,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::Output {
        self.relative().child(vertical_scrollbar_layer(
            scroll_handle,
            ElementId::CodeLocation(*Location::caller()),
            window,
            cx,
        ))
    }

    fn vertical_scrollbar_for_with_id<H: ScrollbarHandle + Clone>(
        self,
        scroll_handle: &H,
        id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::Output {
        self.relative()
            .child(vertical_scrollbar_layer(scroll_handle, id, window, cx))
    }
}

impl WithScrollbar for Stateful<Div> {
    type Output = Self;

    #[track_caller]
    fn vertical_scrollbar_for<H: ScrollbarHandle + Clone>(
        self,
        scroll_handle: &H,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::Output {
        self.relative().child(vertical_scrollbar_layer(
            scroll_handle,
            ElementId::CodeLocation(*Location::caller()),
            window,
            cx,
        ))
    }

    fn vertical_scrollbar_for_with_id<H: ScrollbarHandle + Clone>(
        self,
        scroll_handle: &H,
        id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::Output {
        self.relative()
            .child(vertical_scrollbar_layer(scroll_handle, id, window, cx))
    }
}

fn vertical_scrollbar_layer<H: ScrollbarHandle + Clone>(
    scroll_handle: &H,
    id: impl Into<ElementId>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    if window.is_inspector_picking(cx) {
        return div();
    }

    div()
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .left_0()
        .child(Scrollbar::vertical(scroll_handle).id(id))
}

/// Scrollbar handle for Glyphst's canvas-driven PDF viewport.
///
/// The document canvas owns scrolling physics, so this adapter only exchanges
/// geometry and pending offsets with gpui-component's scrollbar.
#[derive(Clone, Default)]
pub struct ManualScrollHandle(Rc<RefCell<ManualScrollState>>);

#[derive(Default)]
struct ManualScrollState {
    offset: Point<Pixels>,
    max_offset: Point<Pixels>,
    viewport: Bounds<Pixels>,
    pending_offset: Option<Point<Pixels>>,
}

impl ManualScrollHandle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(
        &self,
        viewport: Bounds<Pixels>,
        offset: Point<Pixels>,
        max_offset: Point<Pixels>,
    ) {
        let mut state = self.0.borrow_mut();
        state.viewport = viewport;
        state.offset = offset;
        state.max_offset = max_offset;
    }

    pub fn take_pending_offset(&self) -> Option<Point<Pixels>> {
        self.0.borrow_mut().pending_offset.take()
    }
}

impl ScrollbarHandle for ManualScrollHandle {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.0.borrow().viewport
    }

    fn offset(&self) -> Point<Pixels> {
        self.0.borrow().offset
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        let mut state = self.0.borrow_mut();
        state.offset = offset;
        state.pending_offset = Some(offset);
    }

    fn content_size(&self) -> gpui::Size<Pixels> {
        let state = self.0.borrow();
        size(
            state.viewport.size.width + state.max_offset.x,
            state.viewport.size.height + state.max_offset.y,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use gpui::{
        Context, IntoElement, Modifiers, MouseButton, Render, ScrollHandle, TestAppContext,
        UniformListScrollHandle, VisualTestContext, point, px, uniform_list,
    };
    use gpui_component::Theme;

    use super::*;

    fn draw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
    }

    fn click_scrollbar_track(cx: &mut VisualTestContext, selector: &'static str) {
        let bounds = cx.debug_bounds(selector).expect("scroll host bounds");
        let position = point(bounds.right() - px(4.0), bounds.bottom() - px(8.0));
        cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::default());
        draw(cx);
    }

    struct ScrollHost {
        handle: ScrollHandle,
    }

    impl Render for ScrollHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(100.0))
                .h(px(100.0))
                .debug_selector(|| "scroll-host".to_owned())
                .child(
                    div()
                        .id("scroll-content")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.handle)
                        .children((0..20).map(|_| div().flex_none().w_full().h(px(20.0)))),
                )
                .vertical_scrollbar_for_with_id(&self.handle, "scroll-host-scrollbar", window, cx)
        }
    }

    #[gpui::test]
    fn overlay_scrollbar_track_controls_nested_scroll_area(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            Theme::set_scrollbar_mode(ScrollbarMode::Always, cx);
        });

        let handle = ScrollHandle::new();
        let view_handle = handle.clone();
        let (_, cx) = cx.add_window_view(move |_, _| ScrollHost {
            handle: view_handle,
        });
        let cx: &mut VisualTestContext = cx;
        draw(cx);

        assert_eq!(handle.offset().y, px(0.0));
        assert!(handle.max_offset().y > px(0.0));
        click_scrollbar_track(cx, "scroll-host");
        assert!(handle.offset().y < px(0.0));
    }

    struct UniformListHost {
        handle: UniformListScrollHandle,
    }

    impl Render for UniformListHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(100.0))
                .h(px(100.0))
                .overflow_hidden()
                .debug_selector(|| "uniform-list-host".to_owned())
                .child(
                    uniform_list(
                        "uniform-list",
                        20,
                        cx.processor(|_, visible_range: Range<usize>, _, _| {
                            visible_range
                                .map(|_| div().flex_none().w_full().h(px(20.0)))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.handle)
                    .size_full(),
                )
                .vertical_scrollbar_for_with_id(&self.handle, "uniform-list-scrollbar", window, cx)
        }
    }

    #[gpui::test]
    fn overlay_scrollbar_stays_interactive_for_uniform_list(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            Theme::set_scrollbar_mode(ScrollbarMode::Always, cx);
        });

        let handle = UniformListScrollHandle::new();
        let view_handle = handle.clone();
        let (_, cx) = cx.add_window_view(move |_, _| UniformListHost {
            handle: view_handle,
        });
        let cx: &mut VisualTestContext = cx;
        draw(cx);

        assert_eq!(ScrollbarHandle::offset(&handle).y, px(0.0));
        assert!(ScrollbarHandle::content_size(&handle).height > px(100.0));
        click_scrollbar_track(cx, "uniform-list-host");
        assert!(ScrollbarHandle::offset(&handle).y < px(0.0));
    }
}
