//! Sticky ancestor rows for uniformly sized hierarchical lists.
//!
//! This is adapted from Zed's project-panel sticky-scroll decoration. Callers
//! provide lightweight candidates for the visible list range and render the
//! ancestor rows for the candidate selected as the sticky anchor.

use std::{ops::Range, rc::Rc};

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Context, Element, ElementId, Entity, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Point, Render, Style, UniformListDecoration,
    Window, point, px, size,
};
use smallvec::SmallVec;

/// A hierarchical list entry that can act as a sticky-scroll anchor.
pub trait StickyCandidate {
    /// The zero-based nesting depth of this entry.
    fn depth(&self) -> usize;
}

type ComputeStickyItems<T> = dyn Fn(Range<usize>, &mut Window, &mut App) -> SmallVec<[T; 8]>;
type RenderStickyItems<T> = dyn Fn(T, &mut Window, &mut App) -> SmallVec<[AnyElement; 8]>;

/// A decoration that renders sticky ancestor rows over a uniform list.
pub struct StickyItems<T> {
    compute_fn: Rc<ComputeStickyItems<T>>,
    render_fn: Rc<RenderStickyItems<T>>,
    decorations: Vec<Box<dyn StickyItemsDecoration>>,
}

/// Creates a sticky-ancestor decoration for a view-owned hierarchical list.
pub fn sticky_items<V, T>(
    entity: Entity<V>,
    compute_fn: impl Fn(&mut V, Range<usize>, &mut Window, &mut Context<V>) -> SmallVec<[T; 8]>
    + 'static,
    render_fn: impl Fn(&mut V, T, &mut Window, &mut Context<V>) -> SmallVec<[AnyElement; 8]> + 'static,
) -> StickyItems<T>
where
    V: Render,
    T: StickyCandidate + Clone + 'static,
{
    let entity_compute = entity.clone();
    let entity_render = entity;

    let compute_fn = Rc::new(
        move |range: Range<usize>, window: &mut Window, cx: &mut App| -> SmallVec<[T; 8]> {
            entity_compute.update(cx, |view, cx| compute_fn(view, range, window, cx))
        },
    );
    let render_fn = Rc::new(
        move |entry: T, window: &mut Window, cx: &mut App| -> SmallVec<[AnyElement; 8]> {
            entity_render.update(cx, |view, cx| render_fn(view, entry, window, cx))
        },
    );

    StickyItems {
        compute_fn,
        render_fn,
        decorations: Vec::new(),
    }
}

impl<T> StickyItems<T>
where
    T: StickyCandidate + Clone + 'static,
{
    /// Adds a decoration to the rendered sticky rows.
    pub fn with_decoration(mut self, decoration: impl StickyItemsDecoration + 'static) -> Self {
        self.decorations.push(Box::new(decoration));
        self
    }
}

struct StickyItemsElement {
    drifting_element: Option<AnyElement>,
    drifting_decoration: Option<AnyElement>,
    rest_elements: SmallVec<[AnyElement; 8]>,
    rest_decorations: SmallVec<[AnyElement; 1]>,
}

impl IntoElement for StickyItemsElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for StickyItemsElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (window.request_layout(Style::default(), [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(ref mut drifting_element) = self.drifting_element {
            drifting_element.paint(window, cx);
        }
        if let Some(ref mut drifting_decoration) = self.drifting_decoration {
            drifting_decoration.paint(window, cx);
        }
        for item in self.rest_elements.iter_mut().rev() {
            item.paint(window, cx);
        }
        for item in self.rest_decorations.iter_mut() {
            item.paint(window, cx);
        }
    }
}

impl<T> UniformListDecoration for StickyItems<T>
where
    T: StickyCandidate + Clone + 'static,
{
    fn compute(
        &self,
        visible_range: Range<usize>,
        bounds: Bounds<Pixels>,
        scroll_offset: Point<Pixels>,
        item_height: Pixels,
        _item_count: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let entries = (self.compute_fn)(visible_range.clone(), window, cx);

        let Some(sticky_anchor) = find_sticky_anchor(&entries, visible_range.start) else {
            return StickyItemsElement {
                drifting_element: None,
                drifting_decoration: None,
                rest_elements: SmallVec::new(),
                rest_decorations: SmallVec::new(),
            }
            .into_any_element();
        };

        let anchor_depth = sticky_anchor.entry.depth();
        let mut elements = (self.render_fn)(sticky_anchor.entry, window, cx);
        let items_count = elements.len();

        let indents: SmallVec<[usize; 8]> = (0..items_count)
            .map(|ix| anchor_depth.saturating_sub(items_count.saturating_sub(ix)))
            .collect();

        let mut last_decoration_element = None;
        let mut rest_decoration_elements = SmallVec::new();

        let expanded_width = bounds.size.width + scroll_offset.x.abs();
        let decor_available_space = size(
            AvailableSpace::Definite(expanded_width),
            AvailableSpace::Definite(bounds.size.height),
        );

        let drifting_y_offset = if sticky_anchor.drifting {
            let scroll_top = -scroll_offset.y;
            let anchor_top = item_height * (sticky_anchor.index + 1);
            let sticky_area_height = item_height * items_count;
            (anchor_top - scroll_top - sticky_area_height).min(Pixels::ZERO)
        } else {
            Pixels::ZERO
        };
        let show_drifting_item = sticky_anchor.drifting && drifting_y_offset == Pixels::ZERO;

        let (drifting_indent, rest_indents) = if sticky_anchor.drifting && !indents.is_empty() {
            let last = indents[indents.len() - 1];
            let rest: SmallVec<[usize; 8]> = indents[..indents.len() - 1].iter().copied().collect();
            (show_drifting_item.then_some(last), rest)
        } else {
            (None, indents)
        };

        let base_origin = bounds.origin - point(px(0.), scroll_offset.y);

        for decoration in &self.decorations {
            if let Some(drifting_indent) = drifting_indent {
                let drifting_indent_vec: SmallVec<[usize; 8]> =
                    [drifting_indent].into_iter().collect();
                let sticky_origin = base_origin
                    + point(px(0.), item_height * rest_indents.len() + drifting_y_offset);
                let decoration_bounds = Bounds::new(sticky_origin, bounds.size);

                let mut drifting_decoration = decoration.as_ref().compute(
                    &drifting_indent_vec,
                    decoration_bounds,
                    scroll_offset,
                    item_height,
                    window,
                    cx,
                );
                drifting_decoration.layout_as_root(decor_available_space, window, cx);
                drifting_decoration.prepaint_at(sticky_origin, window, cx);
                last_decoration_element = Some(drifting_decoration);
            }

            if !rest_indents.is_empty() {
                let decoration_bounds = Bounds::new(base_origin, bounds.size);
                let mut rest_decoration = decoration.as_ref().compute(
                    &rest_indents,
                    decoration_bounds,
                    scroll_offset,
                    item_height,
                    window,
                    cx,
                );
                rest_decoration.layout_as_root(decor_available_space, window, cx);
                rest_decoration.prepaint_at(bounds.origin, window, cx);
                rest_decoration_elements.push(rest_decoration);
            }
        }

        let (mut drifting_element, mut rest_elements) =
            if sticky_anchor.drifting && !elements.is_empty() {
                let last = elements.pop().expect("checked non-empty sticky elements");
                (show_drifting_item.then_some(last), elements)
            } else {
                (None, elements)
            };

        let element_available_space = size(
            AvailableSpace::Definite(expanded_width),
            AvailableSpace::Definite(item_height),
        );

        // Hitboxes are inspected in reverse insertion order, so prepaint the
        // drifting row before the stationary rows.
        if let Some(ref mut drifting_element) = drifting_element {
            let sticky_origin = base_origin
                + point(
                    px(0.),
                    item_height * rest_elements.len() + drifting_y_offset,
                );
            drifting_element.layout_as_root(element_available_space, window, cx);
            drifting_element.prepaint_at(sticky_origin, window, cx);
        }

        for (ix, element) in rest_elements.iter_mut().enumerate() {
            let sticky_origin = base_origin + point(px(0.), item_height * ix);
            element.layout_as_root(element_available_space, window, cx);
            element.prepaint_at(sticky_origin, window, cx);
        }

        StickyItemsElement {
            drifting_element,
            drifting_decoration: last_decoration_element,
            rest_elements,
            rest_decorations: rest_decoration_elements,
        }
        .into_any_element()
    }
}

struct StickyAnchor<T> {
    entry: T,
    index: usize,
    drifting: bool,
}

fn find_sticky_anchor<T: StickyCandidate + Clone>(
    entries: &SmallVec<[T; 8]>,
    visible_range_start: usize,
) -> Option<StickyAnchor<T>> {
    let mut entries = entries.iter().enumerate().peekable();
    while let Some((ix, current_entry)) = entries.next() {
        let depth = current_entry.depth();

        if depth < ix {
            return Some(StickyAnchor {
                entry: current_entry.clone(),
                index: visible_range_start + ix,
                drifting: false,
            });
        }

        if let Some(&(_next_ix, next_entry)) = entries.peek() {
            let next_depth = next_entry.depth();
            let next_item_outdented = next_depth + 1 == depth;
            let depth_same_as_index = depth == ix;
            let depth_greater_than_index = depth == ix + 1;

            if next_item_outdented && (depth_same_as_index || depth_greater_than_index) {
                return Some(StickyAnchor {
                    entry: current_entry.clone(),
                    index: visible_range_start + ix,
                    drifting: depth_greater_than_index,
                });
            }
        }
    }

    None
}

/// Additional decoration rendered alongside sticky rows, such as indent
/// guides.
pub trait StickyItemsDecoration {
    /// Computes the decoration for the supplied sticky-row indentation levels.
    fn compute(
        &self,
        indents: &SmallVec<[usize; 8]>,
        bounds: Bounds<Pixels>,
        scroll_offset: Point<Pixels>,
        item_height: Pixels,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrollbar::ScrollbarHandle;
    use gpui::{
        Context, InteractiveElement as _, ParentElement as _, Render, ScrollStrategy, Styled as _,
        TestAppContext, UniformListScrollHandle, VisualTestContext, div, uniform_list,
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Candidate(usize);

    impl StickyCandidate for Candidate {
        fn depth(&self) -> usize {
            self.0
        }
    }

    fn candidates(depths: &[usize]) -> SmallVec<[Candidate; 8]> {
        depths.iter().copied().map(Candidate).collect()
    }

    #[test]
    fn finds_stationary_anchor_after_visible_ancestors() {
        let anchor = find_sticky_anchor(&candidates(&[3, 3, 1]), 8).expect("sticky anchor");

        assert_eq!(anchor.entry, Candidate(1));
        assert_eq!(anchor.index, 10);
        assert!(!anchor.drifting);
    }

    #[test]
    fn marks_last_parent_as_drifting_at_an_outdent() {
        let anchor = find_sticky_anchor(&candidates(&[1, 2, 1]), 4).expect("sticky anchor");

        assert_eq!(anchor.entry, Candidate(2));
        assert_eq!(anchor.index, 5);
        assert!(anchor.drifting);
    }

    #[test]
    fn omits_sticky_anchor_at_the_top_level() {
        assert!(find_sticky_anchor(&candidates(&[0, 1, 2]), 0).is_none());
    }

    #[derive(Clone)]
    struct VisualCandidate {
        index: usize,
        depth: usize,
    }

    impl StickyCandidate for VisualCandidate {
        fn depth(&self) -> usize {
            self.depth
        }
    }

    struct StickyListHost {
        depths: Vec<usize>,
        handle: UniformListScrollHandle,
    }

    impl Render for StickyListHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let item_count = self.depths.len();

            div()
                .w(px(100.0))
                .h(px(100.0))
                .overflow_hidden()
                .debug_selector(|| "sticky-list-host".to_owned())
                .child(
                    uniform_list(
                        "sticky-list",
                        item_count,
                        cx.processor(|_, visible_range: Range<usize>, _, _| {
                            visible_range
                                .map(|_| div().flex_none().w_full().h(px(20.0)))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .with_decoration(sticky_items(
                        cx.entity(),
                        |this, visible_range, _, _| {
                            visible_range
                                .filter_map(|index| {
                                    this.depths
                                        .get(index)
                                        .copied()
                                        .map(|depth| VisualCandidate { index, depth })
                                })
                                .collect()
                        },
                        |_, candidate, _, _| {
                            (0..candidate.depth)
                                .map(|depth| {
                                    let selector =
                                        format!("sticky-ancestor-{}-{depth}", candidate.index);
                                    div()
                                        .flex_none()
                                        .w_full()
                                        .h(px(20.0))
                                        .debug_selector(move || selector.clone())
                                        .into_any_element()
                                })
                                .collect()
                        },
                    ))
                    .track_scroll(&self.handle)
                    .size_full(),
                )
        }
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
    }

    fn assert_y_near(actual: Pixels, expected: Pixels) {
        assert!(
            (actual.as_f32() - expected.as_f32()).abs() < 0.01,
            "expected y {expected:?}, got {actual:?}",
        );
    }

    #[gpui::test]
    fn sticky_rows_stack_and_disappear_atomically_at_an_outdent(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);

        let handle = UniformListScrollHandle::new();
        let view_handle = handle.clone();
        let (_, cx) = cx.add_window_view(move |_, _| StickyListHost {
            depths: vec![0, 1, 2, 3, 3, 3, 2, 1, 0, 0, 0, 0],
            handle: view_handle,
        });
        let cx: &mut VisualTestContext = cx;
        draw(cx);

        handle.scroll_to_item_strict(3, ScrollStrategy::Top);
        draw(cx);

        let host = cx.debug_bounds("sticky-list-host").expect("host bounds");
        for (selector, y) in [
            ("sticky-ancestor-5-0", 0.0),
            ("sticky-ancestor-5-1", 20.0),
            ("sticky-ancestor-5-2", 40.0),
        ] {
            let bounds = cx.debug_bounds(selector).expect("sticky ancestor bounds");
            assert_y_near(bounds.origin.y, host.origin.y + px(y));
        }

        ScrollbarHandle::set_offset(&handle, point(px(0.0), px(-61.0)));
        draw(cx);

        assert!(
            cx.debug_bounds("sticky-ancestor-5-2").is_none(),
            "a drifting sticky row must disappear before it becomes partial",
        );
        assert_y_near(
            cx.debug_bounds("sticky-ancestor-5-0")
                .expect("first stationary ancestor")
                .origin
                .y,
            host.origin.y,
        );
        assert_y_near(
            cx.debug_bounds("sticky-ancestor-5-1")
                .expect("second stationary ancestor")
                .origin
                .y,
            host.origin.y + px(20.0),
        );

        ScrollbarHandle::set_offset(&handle, point(px(0.0), px(-60.0)));
        draw(cx);

        let restored = cx
            .debug_bounds("sticky-ancestor-5-2")
            .expect("restored sticky ancestor bounds");
        assert_y_near(restored.origin.y, host.origin.y + px(40.0));
    }
}
