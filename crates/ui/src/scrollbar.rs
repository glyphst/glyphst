use std::{
    any::Any,
    cell::RefCell,
    ops::Range,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    App, BorderStyle, Bounds, ContentMask, Context, Corners, CursorStyle, DispatchPhase, Div,
    Edges, Element, ElementId, Entity, EntityId, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    IntoElement, IsZero, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Point, Position, Render, ScrollHandle, ScrollWheelEvent, Stateful,
    Style, Task, UniformListDecoration, UniformListScrollHandle, Window, ease_in_out, point, px,
    quad, relative, rgb, size,
};

use crate::settings::SettingsStore;

const SCROLLBAR_HIDE_DELAY_INTERVAL: Duration = Duration::from_secs(1);
const SCROLLBAR_HIDE_DURATION: Duration = Duration::from_millis(400);
const SCROLLBAR_SHOW_DURATION: Duration = Duration::from_millis(50);
const SCROLLBAR_PADDING: Pixels = px(4.0);
const MINIMUM_THUMB_SIZE: Pixels = px(25.0);

pub trait WithScrollbar: Sized {
    type Output;

    #[track_caller]
    fn vertical_scrollbar_for<T: ScrollableHandle + Clone>(
        self,
        scroll_handle: &T,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::Output;
}

impl WithScrollbar for Div {
    type Output = Self;

    #[track_caller]
    fn vertical_scrollbar_for<T: ScrollableHandle + Clone>(
        self,
        scroll_handle: &T,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::Output {
        let state = scrollbar_state(
            ScrollbarConfig::new(scroll_handle.clone())
                .id(core::panic::Location::caller())
                .notify_content(),
            window,
            cx,
        );

        self.child(state)
    }
}

impl WithScrollbar for Stateful<Div> {
    type Output = Self;

    #[track_caller]
    fn vertical_scrollbar_for<T: ScrollableHandle + Clone>(
        self,
        scroll_handle: &T,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::Output {
        let state = scrollbar_state(
            ScrollbarConfig::new(scroll_handle.clone())
                .id(core::panic::Location::caller())
                .notify_content(),
            window,
            cx,
        );

        self.child(state)
    }
}

#[track_caller]
pub fn uniform_list_scrollbar_for<T: ScrollableHandle + Clone>(
    scroll_handle: &T,
    id: impl Into<ElementId>,
    window: &mut Window,
    cx: &mut App,
) -> impl UniformListDecoration + 'static {
    ScrollbarDecoration {
        state: scrollbar_state(
            ScrollbarConfig::new(scroll_handle.clone())
                .id(id)
                .notify_content(),
            window,
            cx,
        ),
    }
}

struct ScrollbarDecoration<T: ScrollableHandle> {
    state: Entity<ScrollbarState<T>>,
}

impl<T: ScrollableHandle> UniformListDecoration for ScrollbarDecoration<T> {
    fn compute(
        &self,
        _visible_range: Range<usize>,
        _bounds: Bounds<Pixels>,
        scroll_offset: Point<Pixels>,
        _item_height: Pixels,
        _item_count: usize,
        _window: &mut Window,
        _cx: &mut App,
    ) -> gpui::AnyElement {
        ScrollbarElement {
            origin: -scroll_offset,
            state: self.state.clone(),
        }
        .into_any_element()
    }
}

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

pub trait ScrollableHandle: 'static + Any + Sized + Clone {
    fn max_offset(&self) -> Point<Pixels>;
    fn set_offset(&self, point: Point<Pixels>);
    fn offset(&self) -> Point<Pixels>;
    fn viewport(&self) -> Bounds<Pixels>;
    fn drag_started(&self) {}
    fn drag_ended(&self) {}
}

impl ScrollableHandle for ScrollHandle {
    fn max_offset(&self) -> Point<Pixels> {
        self.max_offset()
    }

    fn set_offset(&self, point: Point<Pixels>) {
        self.set_offset(point);
    }

    fn offset(&self) -> Point<Pixels> {
        self.offset()
    }

    fn viewport(&self) -> Bounds<Pixels> {
        self.bounds()
    }
}

impl ScrollableHandle for UniformListScrollHandle {
    fn max_offset(&self) -> Point<Pixels> {
        self.0.borrow().base_handle.max_offset()
    }

    fn set_offset(&self, point: Point<Pixels>) {
        self.0.borrow().base_handle.set_offset(point);
    }

    fn offset(&self) -> Point<Pixels> {
        self.0.borrow().base_handle.offset()
    }

    fn viewport(&self) -> Bounds<Pixels> {
        self.0.borrow().base_handle.bounds()
    }
}

impl ScrollableHandle for ManualScrollHandle {
    fn max_offset(&self) -> Point<Pixels> {
        self.0.borrow().max_offset
    }

    fn set_offset(&self, point: Point<Pixels>) {
        let mut state = self.0.borrow_mut();
        state.offset = point;
        state.pending_offset = Some(point);
    }

    fn offset(&self) -> Point<Pixels> {
        self.0.borrow().offset
    }

    fn viewport(&self) -> Bounds<Pixels> {
        self.0.borrow().viewport
    }
}

struct ScrollbarConfig<T: ScrollableHandle> {
    id: Option<ElementId>,
    scroll_handle: T,
    notify_parent: bool,
    width: Pixels,
}

impl<T: ScrollableHandle> ScrollbarConfig<T> {
    fn new(scroll_handle: T) -> Self {
        Self {
            id: None,
            scroll_handle,
            notify_parent: false,
            width: px(6.0),
        }
    }

    fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }

    fn notify_content(mut self) -> Self {
        self.notify_parent = true;
        self
    }
}

fn scrollbar_state<T>(
    mut config: ScrollbarConfig<T>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ScrollbarState<T>>
where
    T: ScrollableHandle,
{
    let element_id = config
        .id
        .take()
        .unwrap_or_else(|| core::panic::Location::caller().into());

    window.use_keyed_state(element_id, cx, move |_window, cx| {
        let parent_id = cx.entity_id();
        let notify_id = config.notify_parent.then_some(parent_id);
        ScrollbarState::new(config.scroll_handle, notify_id, config.width, cx)
    })
}

#[derive(PartialEq, Clone, Debug)]
enum VisibilityState {
    Visible,
    Animating { showing: bool, delta: f32 },
    Hidden,
}

const DELTA_MAX: f32 = 1.0;

impl VisibilityState {
    fn for_show() -> Self {
        Self::Animating {
            showing: true,
            delta: Default::default(),
        }
    }

    fn for_autohide() -> Self {
        Self::Animating {
            showing: Default::default(),
            delta: Default::default(),
        }
    }

    fn is_visible(&self) -> bool {
        matches!(self, Self::Visible | Self::Animating { .. })
    }

    fn animation_progress(&self) -> Option<(f32, Duration, bool)> {
        match self {
            Self::Animating { showing, delta } => Some((
                *delta,
                if *showing {
                    SCROLLBAR_SHOW_DURATION
                } else {
                    SCROLLBAR_HIDE_DURATION
                },
                *showing,
            )),
            _ => None,
        }
    }

    fn set_delta(&mut self, new_delta: f32) {
        match self {
            Self::Animating { showing, .. } if new_delta >= DELTA_MAX => {
                if *showing {
                    *self = Self::Visible;
                } else {
                    *self = Self::Hidden;
                }
            }
            Self::Animating { delta, .. } => *delta = new_delta,
            _ => {}
        }
    }

    fn toggle_visible(&self) -> Self {
        match self {
            Self::Hidden => Self::for_show(),
            Self::Animating {
                showing: false,
                delta: progress,
            } => Self::Animating {
                showing: true,
                delta: DELTA_MAX - progress,
            },
            _ => self.clone(),
        }
    }
}

enum ParentHoverEvent {
    Within,
    Entered,
    Exited,
    Outside,
}

struct ScrollbarState<T: ScrollableHandle> {
    thumb_state: ThumbState,
    notify_id: Option<EntityId>,
    scroll_handle: T,
    width: Pixels,
    show_state: VisibilityState,
    mouse_in_parent: bool,
    last_prepaint_state: Option<ScrollbarPrepaintState>,
    _auto_hide_task: Option<Task<()>>,
}

impl<T: ScrollableHandle> ScrollbarState<T> {
    fn new(
        scroll_handle: T,
        notify_id: Option<EntityId>,
        width: Pixels,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            thumb_state: Default::default(),
            notify_id,
            scroll_handle,
            width,
            show_state: VisibilityState::for_show(),
            mouse_in_parent: true,
            last_prepaint_state: None,
            _auto_hide_task: None,
        }
    }

    fn schedule_auto_hide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self._auto_hide_task.is_none() && self.visible() {
            self._auto_hide_task = Some(cx.spawn_in(window, async move |scrollbar_state, cx| {
                cx.background_executor()
                    .timer(SCROLLBAR_HIDE_DELAY_INTERVAL)
                    .await;
                let _ = scrollbar_state.update(cx, |state, cx| {
                    if state.thumb_state == ThumbState::Inactive {
                        state.set_visibility(VisibilityState::for_autohide(), cx);
                    }
                    state._auto_hide_task.take();
                });
            }));
        }
    }

    fn show_scrollbar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_visibility(self.show_state.toggle_visible(), cx);
        self._auto_hide_task.take();
        self.schedule_auto_hide(window, cx);
    }

    fn set_visibility(&mut self, visibility: VisibilityState, cx: &mut Context<Self>) {
        if self.show_state != visibility {
            self.show_state = visibility;
            cx.notify();
        }
    }

    fn set_offset(&mut self, offset: Point<Pixels>, cx: &mut Context<Self>) {
        self.scroll_handle.set_offset(offset);
        self.notify_parent(cx);
        cx.notify();
    }

    fn is_dragging(&self) -> bool {
        self.thumb_state.is_dragging()
    }

    fn set_dragging(&mut self, drag_offset: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        self.set_thumb_state(ThumbState::Dragging(drag_offset), window, cx);
        self.scroll_handle.drag_started();
    }

    fn update_hovered_thumb(
        &mut self,
        position: &Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_thumb_state(
            if self
                .last_prepaint_state
                .as_ref()
                .and_then(|state| state.thumb_for_position(position))
                .is_some_and(|thumb| thumb.cursor_hitbox.is_hovered(window))
            {
                ThumbState::Hover
            } else {
                ThumbState::Inactive
            },
            window,
            cx,
        );
    }

    fn set_thumb_state(&mut self, state: ThumbState, window: &mut Window, cx: &mut Context<Self>) {
        if self.thumb_state != state {
            if state == ThumbState::Inactive {
                self.schedule_auto_hide(window, cx);
            } else {
                self.set_visibility(self.show_state.toggle_visible(), cx);
                self._auto_hide_task.take();
            }
            self.thumb_state = state;
            cx.notify();
        }
    }

    fn update_parent_hovered(&mut self, window: &Window) -> ParentHoverEvent {
        let last_parent_hovered = self.mouse_in_parent;
        self.mouse_in_parent = self.parent_hovered(window);
        let state_changed = self.mouse_in_parent != last_parent_hovered;
        match (self.mouse_in_parent, state_changed) {
            (true, true) => ParentHoverEvent::Entered,
            (true, false) => ParentHoverEvent::Within,
            (false, true) => ParentHoverEvent::Exited,
            (false, false) => ParentHoverEvent::Outside,
        }
    }

    fn parent_hovered(&self, window: &Window) -> bool {
        self.last_prepaint_state
            .as_ref()
            .is_some_and(|state| state.parent_bounds_hitbox.is_hovered(window))
    }

    fn hit_for_position(&self, position: &Point<Pixels>) -> Option<&ScrollbarLayout> {
        self.last_prepaint_state
            .as_ref()
            .and_then(|state| state.hit_for_position(position))
    }

    fn thumb_layout(&self) -> Option<&ScrollbarLayout> {
        self.last_prepaint_state
            .as_ref()
            .and_then(|state| state.thumb.as_ref())
    }

    fn thumb_range(&self) -> Option<Range<f32>> {
        let max_offset = self.scroll_handle.max_offset().y;
        let viewport_height = self.scroll_handle.viewport().size.height;
        if max_offset.is_zero() || viewport_height.is_zero() {
            return None;
        }

        let content_height = viewport_height + max_offset;
        let visible_percentage = viewport_height / content_height;
        let thumb_size = MINIMUM_THUMB_SIZE.max(viewport_height * visible_percentage);
        if thumb_size > viewport_height {
            return None;
        }

        let current_offset = self
            .scroll_handle
            .offset()
            .y
            .clamp(-max_offset, Pixels::ZERO)
            .abs();
        let start_offset = (current_offset / max_offset) * (viewport_height - thumb_size);
        Some((start_offset / viewport_height)..((start_offset + thumb_size) / viewport_height))
    }

    fn visible(&self) -> bool {
        self.show_state.is_visible()
    }

    fn notify_parent(&self, cx: &mut App) {
        if let Some(entity_id) = self.notify_id {
            cx.notify(entity_id);
        }
    }
}

impl<T: ScrollableHandle> Render for ScrollbarState<T> {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        ScrollbarElement {
            origin: Default::default(),
            state: cx.entity(),
        }
    }
}

struct ScrollbarElement<T: ScrollableHandle> {
    origin: Point<Pixels>,
    state: Entity<ScrollbarState<T>>,
}

#[derive(Default, Debug, PartialEq, Eq)]
enum ThumbState {
    #[default]
    Inactive,
    Hover,
    Dragging(Pixels),
}

impl ThumbState {
    fn is_dragging(&self) -> bool {
        matches!(*self, ThumbState::Dragging(..))
    }
}

enum ScrollbarMouseEvent {
    TrackClick,
    ThumbDrag(Pixels),
}

struct ScrollbarLayout {
    thumb_bounds: Bounds<Pixels>,
    track_bounds: Bounds<Pixels>,
    cursor_hitbox: Hitbox,
}

impl ScrollbarLayout {
    fn compute_click_offset(
        &self,
        event_position: Point<Pixels>,
        max_offset: Point<Pixels>,
        event_type: ScrollbarMouseEvent,
    ) -> Pixels {
        let viewport_height = self.track_bounds.size.height;
        let thumb_height = self.thumb_bounds.size.height;
        let thumb_offset = match event_type {
            ScrollbarMouseEvent::TrackClick => thumb_height / 2.0,
            ScrollbarMouseEvent::ThumbDrag(thumb_offset) => thumb_offset,
        };

        let thumb_start = (event_position.y - self.track_bounds.origin.y - thumb_offset)
            .clamp(px(0.0), viewport_height - thumb_height);
        let percentage = if viewport_height > thumb_height {
            thumb_start / (viewport_height - thumb_height)
        } else {
            0.0
        };

        -max_offset.y * percentage
    }
}

impl PartialEq for ScrollbarLayout {
    fn eq(&self, other: &Self) -> bool {
        self.thumb_bounds.origin.y - self.track_bounds.origin.y
            == other.thumb_bounds.origin.y - other.track_bounds.origin.y
            && self.thumb_bounds.size.height == other.thumb_bounds.size.height
    }
}

struct ScrollbarPrepaintState {
    parent_bounds_hitbox: Hitbox,
    thumb: Option<ScrollbarLayout>,
}

impl ScrollbarPrepaintState {
    fn thumb_for_position(&self, position: &Point<Pixels>) -> Option<&ScrollbarLayout> {
        self.thumb
            .as_ref()
            .filter(|info| info.thumb_bounds.contains(position))
    }

    fn hit_for_position(&self, position: &Point<Pixels>) -> Option<&ScrollbarLayout> {
        self.thumb
            .as_ref()
            .filter(|info| info.track_bounds.contains(position))
    }
}

impl PartialEq for ScrollbarPrepaintState {
    fn eq(&self, other: &Self) -> bool {
        self.thumb == other.thumb
    }
}

impl<T: ScrollableHandle> Element for ScrollbarElement<T> {
    type RequestLayoutState = ();
    type PrepaintState = Option<(ScrollbarPrepaintState, Option<f32>)>;

    fn id(&self) -> Option<ElementId> {
        Some(("scrollbar_animation", self.state.entity_id()).into())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let scrollbar_style = Style {
            position: Position::Absolute,
            inset: Edges::default(),
            size: size(relative(1.0), relative(1.0)).map(Into::into),
            ..Default::default()
        };

        (window.request_layout(scrollbar_style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let prepaint_state = ScrollbarPrepaintState {
            thumb: self.state.read(cx).thumb_range().map(|thumb_range| {
                let width = self.state.read(cx).width;
                let track_height = (bounds.size.height - SCROLLBAR_PADDING * 2.0).max(px(0.0));
                let track_bounds = Bounds::new(
                    point(
                        self.origin.x + bounds.right() - SCROLLBAR_PADDING - width,
                        self.origin.y + bounds.top() + SCROLLBAR_PADDING,
                    ),
                    size(width, track_height),
                );
                let available_height = track_bounds.size.height;
                let thumb_start = thumb_range.start * available_height;
                let thumb_end = thumb_range.end * available_height;
                let thumb_bounds = Bounds::new(
                    point(track_bounds.origin.x, track_bounds.origin.y + thumb_start),
                    size(width, thumb_end - thumb_start),
                );

                ScrollbarLayout {
                    thumb_bounds,
                    track_bounds,
                    cursor_hitbox: window
                        .insert_hitbox(track_bounds, HitboxBehavior::BlockMouseExceptScroll),
                }
            }),
            parent_bounds_hitbox: window.insert_hitbox(bounds, HitboxBehavior::Normal),
        };

        if Some(&prepaint_state) != self.state.read(cx).last_prepaint_state.as_ref() {
            self.state
                .update(cx, |state, cx| state.show_scrollbar(window, cx));
        }

        let autohide_delta = self.state.read(cx).show_state.animation_progress().map(
            |(delta, delta_duration, should_invert)| {
                window.with_element_state(id.unwrap(), |state, window| {
                    let state = state.unwrap_or_else(Instant::now);
                    let current = Instant::now();

                    let new_delta =
                        DELTA_MAX.min(delta + (current - state).div_duration_f32(delta_duration));
                    self.state
                        .update(cx, |state, _| state.show_state.set_delta(new_delta));

                    window.request_animation_frame();
                    let delta = if should_invert {
                        DELTA_MAX - delta
                    } else {
                        delta
                    };
                    (ease_in_out(delta), current)
                })
            },
        );

        Some((prepaint_state, autohide_delta))
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        Bounds { origin, size }: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint_state: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some((prepaint_state, autohide_fade)) = prepaint_state.take() else {
            return;
        };

        let bounds = Bounds::new(self.origin + origin, size);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            let mut capture_phase = DispatchPhase::Bubble;

            if self.state.read(cx).visible()
                && let Some(ScrollbarLayout {
                    thumb_bounds,
                    cursor_hitbox,
                    ..
                }) = prepaint_state.thumb.as_ref()
            {
                let thumb_state = &self.state.read(cx).thumb_state;
                capture_phase = if thumb_state.is_dragging() {
                    DispatchPhase::Capture
                } else {
                    DispatchPhase::Bubble
                };

                let theme = cx.global::<SettingsStore>().resolved_theme;
                let surface_color: Hsla = rgb(theme.surface_background).into();
                let base_color: Hsla = rgb(theme.text_muted).into();
                let hover_color: Hsla = rgb(theme.text).into();
                let active_color: Hsla = rgb(theme.accent).into();
                let thumb_base = match thumb_state {
                    ThumbState::Dragging(_) => active_color.alpha(0.72),
                    ThumbState::Hover => hover_color.alpha(0.64),
                    ThumbState::Inactive => base_color.alpha(0.52),
                };
                let mut thumb_color = surface_color.blend(thumb_base);
                if matches!(thumb_state, ThumbState::Inactive)
                    && let Some(fade) = autohide_fade
                {
                    thumb_color.fade_out(fade);
                }

                window.paint_quad(quad(
                    *thumb_bounds,
                    Corners::all(Pixels::MAX).clamp_radii_for_quad_size(thumb_bounds.size),
                    thumb_color,
                    Edges::default(),
                    Hsla::transparent_black(),
                    BorderStyle::default(),
                ));

                if thumb_state.is_dragging() {
                    window.set_window_cursor_style(CursorStyle::Arrow);
                } else {
                    window.set_cursor_style(CursorStyle::Arrow, cursor_hitbox);
                }
            }

            self.state.update(cx, |state, _| {
                state.last_prepaint_state = Some(prepaint_state)
            });

            window.on_mouse_event({
                let state = self.state.clone();

                move |event: &MouseDownEvent, phase, window, cx| {
                    state.update(cx, |state, cx| {
                        let Some(scrollbar_layout) = (phase == capture_phase
                            && event.button == MouseButton::Left)
                            .then(|| state.hit_for_position(&event.position))
                            .flatten()
                        else {
                            return;
                        };

                        if scrollbar_layout.thumb_bounds.contains(&event.position) {
                            let offset = event.position.y - scrollbar_layout.thumb_bounds.origin.y;
                            state.set_dragging(offset, window, cx);
                        } else {
                            let scroll_handle = &state.scroll_handle;
                            let click_offset = scrollbar_layout.compute_click_offset(
                                event.position,
                                scroll_handle.max_offset(),
                                ScrollbarMouseEvent::TrackClick,
                            );
                            let offset = scroll_handle.offset();
                            state.set_offset(point(offset.x, click_offset), cx);
                        }

                        cx.stop_propagation();
                    });
                }
            });

            window.on_mouse_event({
                let state = self.state.clone();

                move |event: &ScrollWheelEvent, phase, window, cx| {
                    state.update(cx, |state, cx| {
                        if phase.capture() && state.parent_hovered(window) {
                            state.show_scrollbar(window, cx);
                            state.update_hovered_thumb(&event.position, window, cx);
                        }
                    });
                }
            });

            window.on_mouse_event({
                let state = self.state.clone();

                move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase != capture_phase {
                        return;
                    }

                    match state.read(cx).thumb_state {
                        ThumbState::Dragging(drag_state) if event.dragging() => {
                            if let Some(scrollbar_layout) = state.read(cx).thumb_layout() {
                                let scroll_handle = &state.read(cx).scroll_handle;
                                let drag_offset = scrollbar_layout.compute_click_offset(
                                    event.position,
                                    scroll_handle.max_offset(),
                                    ScrollbarMouseEvent::ThumbDrag(drag_state),
                                );
                                let offset = scroll_handle.offset();
                                let new_offset = point(offset.x, drag_offset);

                                state.update(cx, |state, cx| state.set_offset(new_offset, cx));
                                cx.stop_propagation();
                            }
                        }
                        _ => state.update(cx, |state, cx| {
                            match state.update_parent_hovered(window) {
                                hover @ ParentHoverEvent::Entered
                                | hover @ ParentHoverEvent::Within
                                    if event.pressed_button.is_none() =>
                                {
                                    if matches!(hover, ParentHoverEvent::Entered) {
                                        state.show_scrollbar(window, cx);
                                    }
                                    state.update_hovered_thumb(&event.position, window, cx);
                                    if state.thumb_state != ThumbState::Inactive {
                                        cx.stop_propagation();
                                    }
                                }
                                ParentHoverEvent::Exited => {
                                    state.set_thumb_state(ThumbState::Inactive, window, cx);
                                }
                                _ => {}
                            }
                        }),
                    }
                }
            });

            window.on_mouse_event({
                let state = self.state.clone();
                move |event: &MouseUpEvent, phase, window, cx| {
                    if phase != capture_phase {
                        return;
                    }

                    state.update(cx, |state, cx| {
                        if state.is_dragging() {
                            state.scroll_handle.drag_ended();
                        }

                        if !state.parent_hovered(window) {
                            state.schedule_auto_hide(window, cx);
                            return;
                        }

                        state.update_hovered_thumb(&event.position, window, cx);
                    });
                }
            });
        })
    }
}

impl<T: ScrollableHandle> IntoElement for ScrollbarElement<T> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}
