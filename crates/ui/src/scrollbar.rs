use std::{cell::RefCell, ops::Range, rc::Rc};

use gpui::{
    AnyElement, App, Bounds, Div, ElementId, IntoElement as _, Pixels, Point, Stateful,
    UniformListDecoration, Window, div, prelude::*, size,
};

pub use gpui_component::scroll::{
    ScrollableElement, Scrollbar, ScrollbarAxis, ScrollbarHandle, ScrollbarShow,
};

/// Compatibility builder for Glyphst surfaces that already own their scroll
/// area and handle. The rendered control is gpui-component's [`Scrollbar`].
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

    fn vertical_scrollbar_for<H: ScrollbarHandle + Clone>(
        self,
        scroll_handle: &H,
        _: &mut Window,
        _: &mut App,
    ) -> Self::Output {
        self.relative().child(Scrollbar::vertical(scroll_handle))
    }

    fn vertical_scrollbar_for_with_id<H: ScrollbarHandle + Clone>(
        self,
        scroll_handle: &H,
        id: impl Into<ElementId>,
        _: &mut Window,
        _: &mut App,
    ) -> Self::Output {
        self.relative()
            .child(Scrollbar::vertical(scroll_handle).id(id))
    }
}

impl WithScrollbar for Stateful<Div> {
    type Output = Self;

    fn vertical_scrollbar_for<H: ScrollbarHandle + Clone>(
        self,
        scroll_handle: &H,
        _: &mut Window,
        _: &mut App,
    ) -> Self::Output {
        self.relative().child(Scrollbar::vertical(scroll_handle))
    }

    fn vertical_scrollbar_for_with_id<H: ScrollbarHandle + Clone>(
        self,
        scroll_handle: &H,
        id: impl Into<ElementId>,
        _: &mut Window,
        _: &mut App,
    ) -> Self::Output {
        self.relative()
            .child(Scrollbar::vertical(scroll_handle).id(id))
    }
}

/// Attach gpui-component's scrollbar to a GPUI `uniform_list` decoration.
///
/// `uniform_list` paints decorations in content coordinates, so the wrapper
/// cancels the current scroll offset before rendering the component control.
pub fn uniform_list_scrollbar_for<H: ScrollbarHandle + Clone>(
    scroll_handle: &H,
    id: impl Into<ElementId>,
    _: &mut Window,
    _: &mut App,
) -> impl UniformListDecoration + 'static {
    ComponentScrollbarDecoration {
        scroll_handle: scroll_handle.clone(),
        id: id.into(),
    }
}

struct ComponentScrollbarDecoration<H: ScrollbarHandle + Clone> {
    scroll_handle: H,
    id: ElementId,
}

impl<H: ScrollbarHandle + Clone> UniformListDecoration for ComponentScrollbarDecoration<H> {
    fn compute(
        &self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        scroll_offset: Point<Pixels>,
        _: Pixels,
        _: usize,
        _: &mut Window,
        _: &mut App,
    ) -> AnyElement {
        div()
            .absolute()
            .top(-scroll_offset.y)
            .left(-scroll_offset.x)
            .w(bounds.size.width)
            .h(bounds.size.height)
            .child(Scrollbar::vertical(&self.scroll_handle).id(self.id.clone()))
            .into_any_element()
    }
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
