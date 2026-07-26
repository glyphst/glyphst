use super::*;

pub(super) fn pane_axis(
    axis: Axis,
    basis: usize,
    flexes: Rc<RefCell<Vec<f32>>>,
    bounding_boxes: Rc<RefCell<Vec<Option<Bounds<Pixels>>>>>,
    theme: Theme,
    dim_inactive_panes: bool,
) -> PaneAxisElement {
    PaneAxisElement {
        axis,
        basis,
        flexes,
        bounding_boxes,
        children: SmallVec::new(),
        active_pane_ix: None,
        is_leaf_pane_mask: Vec::new(),
        theme,
        dim_inactive_panes,
    }
}

pub(super) struct PaneAxisElement {
    axis: Axis,
    basis: usize,
    flexes: Rc<RefCell<Vec<f32>>>,
    bounding_boxes: Rc<RefCell<Vec<Option<Bounds<Pixels>>>>>,
    children: SmallVec<[AnyElement; 2]>,
    active_pane_ix: Option<usize>,
    is_leaf_pane_mask: Vec<bool>,
    theme: Theme,
    dim_inactive_panes: bool,
}

pub(super) struct PaneAxisLayout {
    drag: Rc<RefCell<Option<ResizeDrag>>>,
    children: Vec<PaneAxisChildLayout>,
}

struct PaneAxisChildLayout {
    bounds: Bounds<Pixels>,
    element: AnyElement,
    handle: Option<PaneAxisHandleLayout>,
    is_leaf_pane: bool,
}

struct PaneAxisHandleLayout {
    hitbox: Hitbox,
    divider_bounds: Bounds<Pixels>,
}

struct ResizeComputation<'a> {
    flexes: Rc<RefCell<Vec<f32>>>,
    event: &'a MouseMoveEvent,
    axis: Axis,
    drag: ResizeDrag,
}

#[derive(Clone)]
struct ResizeDrag {
    ix: usize,
    pointer_origin: Point<Pixels>,
    sizes: Vec<f32>,
    container_along: f32,
}

pub(super) fn resize_axis_sizes(
    sizes: &mut [f32],
    ix: usize,
    requested_delta: f32,
    min_size: f32,
) -> f32 {
    if ix + 1 >= sizes.len() || requested_delta == 0.0 {
        return 0.0;
    }

    if requested_delta.is_sign_positive() {
        let available = sizes[ix + 1..]
            .iter()
            .map(|size| (size - min_size).max(0.0))
            .sum::<f32>();
        let applied = requested_delta.min(available);
        let mut remaining = applied;
        for size in &mut sizes[ix + 1..] {
            let reduction = (*size - min_size).max(0.0).min(remaining);
            *size -= reduction;
            remaining -= reduction;
            if remaining <= f32::EPSILON {
                break;
            }
        }
        sizes[ix] += applied;
        applied
    } else {
        let applied = (-requested_delta).min((sizes[ix] - min_size).max(0.0));
        sizes[ix] -= applied;
        sizes[ix + 1] += applied;
        -applied
    }
}

impl PaneAxisElement {
    pub(super) fn with_active_pane(mut self, active_pane_ix: Option<usize>) -> Self {
        self.active_pane_ix = active_pane_ix;
        self
    }

    pub(super) fn with_is_leaf_pane_mask(mut self, mask: Vec<bool>) -> Self {
        self.is_leaf_pane_mask = mask;
        self
    }

    fn compute_resize(args: ResizeComputation<'_>, window: &mut Window, cx: &mut App) {
        let ResizeComputation {
            flexes,
            event,
            axis,
            drag,
        } = args;
        let min_size = match axis {
            Axis::Horizontal => HORIZONTAL_MIN_SIZE,
            Axis::Vertical => VERTICAL_MIN_SIZE,
        };
        let container_along = drag.container_along;
        if container_along <= 0.0 {
            return;
        }

        let mut flexes = flexes.borrow_mut();
        if flexes.len() != drag.sizes.len() || drag.ix + 1 >= flexes.len() {
            return;
        }

        let len = flexes.len();
        let mut sizes = drag.sizes;
        let requested_delta = (event.position - drag.pointer_origin).along(axis).as_f32();
        resize_axis_sizes(&mut sizes, drag.ix, requested_delta, min_size);

        for (flex, size) in flexes.iter_mut().zip(sizes) {
            *flex = len as f32 * size / container_along;
        }

        window.refresh();
        cx.stop_propagation();
    }

    fn layout_handle(
        axis: Axis,
        pane_bounds: Bounds<Pixels>,
        window: &mut Window,
    ) -> PaneAxisHandleLayout {
        let handle_bounds = Bounds {
            origin: pane_bounds.origin.apply_along(axis, |origin| {
                origin + pane_bounds.size.along(axis) - px(HANDLE_HITBOX_SIZE / 2.0)
            }),
            size: pane_bounds
                .size
                .apply_along(axis, |_| px(HANDLE_HITBOX_SIZE)),
        };
        let divider_bounds = Bounds {
            origin: pane_bounds
                .origin
                .apply_along(axis, |origin| origin + pane_bounds.size.along(axis)),
            size: pane_bounds.size.apply_along(axis, |_| px(DIVIDER_SIZE)),
        };

        PaneAxisHandleLayout {
            hitbox: window.insert_hitbox(handle_bounds, HitboxBehavior::BlockMouse),
            divider_bounds,
        }
    }
}

impl IntoElement for PaneAxisElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for PaneAxisElement {
    type RequestLayoutState = ();
    type PrepaintState = PaneAxisLayout;

    fn id(&self) -> Option<ElementId> {
        Some(self.basis.into())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            flex_basis: relative(0.0).into(),
            size: size(relative(1.0), relative(1.0)).map(Into::into),
            ..Style::default()
        };
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let drag = window.with_element_state::<Rc<RefCell<Option<ResizeDrag>>>, _>(
            global_id.unwrap(),
            |state, _window| {
                let state = state.unwrap_or_else(|| Rc::new(RefCell::new(None)));
                (state.clone(), state)
            },
        );
        let flexes = self.flexes.borrow().clone();
        let len = self.children.len();
        let total_flex = len.max(1) as f32;
        let mut origin = bounds.origin;
        let space_per_flex = bounds.size.along(self.axis) / total_flex;
        let mut bounding_boxes = self.bounding_boxes.borrow_mut();
        bounding_boxes.clear();

        let mut layout = PaneAxisLayout {
            drag,
            children: Vec::new(),
        };

        for (ix, mut child) in std::mem::take(&mut self.children).into_iter().enumerate() {
            let child_flex = flexes.get(ix).copied().unwrap_or(1.0);
            let child_size = bounds
                .size
                .apply_along(self.axis, |_| space_per_flex * child_flex)
                .map(|dimension| dimension.round());
            let child_bounds = Bounds {
                origin,
                size: child_size,
            };
            bounding_boxes.push(Some(child_bounds));
            child.layout_as_root(child_size.into(), window, cx);
            child.prepaint_at(origin, window, cx);
            origin = origin.apply_along(self.axis, |value| value + child_size.along(self.axis));
            let is_leaf_pane = self.is_leaf_pane_mask.get(ix).copied().unwrap_or(true);
            layout.children.push(PaneAxisChildLayout {
                bounds: child_bounds,
                element: child,
                handle: None,
                is_leaf_pane,
            });
        }

        for (ix, child_layout) in layout.children.iter_mut().enumerate() {
            if ix < len.saturating_sub(1) {
                child_layout.handle =
                    Some(Self::layout_handle(self.axis, child_layout.bounds, window));
            }
        }

        layout
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        layout: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        for child in &mut layout.children {
            child.element.paint(window, cx);
        }

        for (ix, child) in layout.children.iter_mut().enumerate() {
            let overlay_bounds = Bounds {
                origin: child
                    .bounds
                    .origin
                    .apply_along(Axis::Horizontal, |value| value + px(1.0)),
                size: child
                    .bounds
                    .size
                    .apply_along(Axis::Horizontal, |value| value - px(1.0)),
            };

            if self.dim_inactive_panes && child.is_leaf_pane && self.active_pane_ix != Some(ix) {
                window.paint_quad(fill(overlay_bounds, self.theme.overlay));
            }

            if child.is_leaf_pane && self.active_pane_ix == Some(ix) {
                window.paint_quad(quad(
                    overlay_bounds,
                    0.0,
                    transparent_black(),
                    px(1.0),
                    self.theme.primary,
                    BorderStyle::Solid,
                ));
            }

            if let Some(handle) = child.handle.as_mut() {
                let cursor_style = match self.axis {
                    Axis::Vertical => CursorStyle::ResizeRow,
                    Axis::Horizontal => CursorStyle::ResizeColumn,
                };

                if layout
                    .drag
                    .borrow()
                    .as_ref()
                    .is_some_and(|drag| drag.ix == ix)
                {
                    window.set_window_cursor_style(cursor_style);
                } else {
                    window.set_cursor_style(cursor_style, &handle.hitbox);
                }

                window.paint_quad(fill(handle.divider_bounds, self.theme.border));

                window.on_mouse_event({
                    let drag = layout.drag.clone();
                    let flexes = self.flexes.clone();
                    let handle_hitbox = handle.hitbox.clone();
                    let axis = self.axis;
                    let container_size = bounds.size;
                    move |event: &MouseDownEvent, phase: DispatchPhase, window, cx| {
                        if phase.bubble() && handle_hitbox.is_hovered(window) {
                            if event.click_count >= 2 {
                                let len = flexes.borrow().len();
                                *flexes.borrow_mut() = vec![1.0; len];
                                window.refresh();
                            } else {
                                let flexes = flexes.borrow();
                                let len = flexes.len();
                                let container_along = container_size.along(axis).as_f32();
                                if len < 2 || container_along <= 0.0 {
                                    return;
                                }
                                let sizes = flexes
                                    .iter()
                                    .map(|flex| container_along * *flex / len as f32)
                                    .collect();
                                drag.replace(Some(ResizeDrag {
                                    ix,
                                    pointer_origin: event.position,
                                    sizes,
                                    container_along,
                                }));
                            }
                            cx.stop_propagation();
                        }
                    }
                });

                window.on_mouse_event({
                    let drag = layout.drag.clone();
                    let flexes = self.flexes.clone();
                    let axis = self.axis;
                    move |event: &MouseMoveEvent, phase: DispatchPhase, window, cx| {
                        let Some(drag_state) = drag.borrow().clone() else {
                            return;
                        };
                        if phase.bubble() && drag_state.ix == ix {
                            Self::compute_resize(
                                ResizeComputation {
                                    flexes: flexes.clone(),
                                    event,
                                    axis,
                                    drag: drag_state,
                                },
                                window,
                                cx,
                            );
                        }
                    }
                });
            }
        }

        window.on_mouse_event({
            let drag = layout.drag.clone();
            move |_: &MouseUpEvent, phase: DispatchPhase, _, _| {
                if phase.bubble() {
                    drag.replace(None);
                }
            }
        });
    }
}

impl ParentElement for PaneAxisElement {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}
