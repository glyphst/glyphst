use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, Pixels, Task, Window, deferred, div, prelude::*, px};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _, ThemeColor,
    button::{Button, ButtonVariants as _},
    status_bar::StatusBar,
};

use super::*;
use crate::{
    document_view::{
        DocumentDockSnapshot, DocumentSidebar, SIDEBAR_MIN_WIDTH, ToggleAiPanel, ToggleNotes,
        ToggleThumbnailSidebar, ToggleToc, default_sidebar_widths, sanitize_sidebar_widths,
    },
    icons::IconName,
    library::{LibraryStore, SidebarWidths},
};
use glyphst_ui::{
    motion::{LAYOUT, ScalarTween},
    style::{SURFACE_GAP, SURFACE_INSET},
};

const DOCK_RESIZE_HANDLE_SIZE: Pixels = px(6.0);
const DOCK_SEPARATOR_WIDTH: f32 = 1.0;
const TOOL_RAIL_WIDTH: f32 = 44.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkspaceDockPosition {
    Left,
    Right,
}

#[derive(Clone)]
struct DraggedWorkspaceDock(WorkspaceDockPosition);

impl Render for DraggedWorkspaceDock {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

pub(super) struct WorkspaceDockState {
    pub(super) left_sidebar: Option<DocumentSidebar>,
    pub(super) ai_panel_open: bool,
    pub(super) preferred_widths: SidebarWidths,
    rendered_left_sidebar: Option<DocumentSidebar>,
    render_ai_panel: bool,
    left_width: ScalarTween,
    right_width: ScalarTween,
    width_save_task: Option<Task<()>>,
}

impl WorkspaceDockState {
    pub(super) fn new(cx: &mut Context<Workspace>) -> Self {
        let preferred_widths = cx
            .global::<LibraryStore>()
            .sidebar_widths()
            .map(sanitize_sidebar_widths)
            .unwrap_or_else(default_sidebar_widths);

        Self {
            left_sidebar: None,
            ai_panel_open: false,
            preferred_widths,
            rendered_left_sidebar: None,
            render_ai_panel: false,
            left_width: ScalarTween::new(0.0, LAYOUT),
            right_width: ScalarTween::new(0.0, LAYOUT),
            width_save_task: None,
        }
    }

    pub(super) fn snapshot(
        &self,
        workspace_width: f32,
        docks_visible: bool,
        tool_rail_visible: bool,
    ) -> DocumentDockSnapshot {
        let mut widths = self.preferred_widths;
        let left_sidebar = docks_visible.then_some(self.left_sidebar).flatten();
        let ai_panel_open = docks_visible && self.ai_panel_open;
        let left_preferred = left_sidebar.map(|sidebar| sidebar.width(self.preferred_widths));
        let right_preferred = ai_panel_open.then_some(self.preferred_widths.ai);
        let tool_rail_width = if tool_rail_visible {
            TOOL_RAIL_WIDTH
        } else {
            0.0
        };
        let (left_width, right_width) = fit_dock_widths(
            left_preferred,
            right_preferred,
            workspace_width - tool_rail_width,
        );

        if let (Some(sidebar), Some(width)) = (left_sidebar, left_width) {
            sidebar.set_width(&mut widths, width);
        }
        if let Some(width) = right_width {
            widths.ai = width;
        }

        DocumentDockSnapshot {
            left_sidebar,
            ai_panel_open,
            tool_rail_width,
            widths,
            layout_motion_active: false,
        }
    }

    fn update_motion(
        &mut self,
        snapshot: DocumentDockSnapshot,
        now: Instant,
        reduce_motion: bool,
    ) -> (f32, f32, bool) {
        if let Some(sidebar) = snapshot.left_sidebar {
            self.rendered_left_sidebar = Some(sidebar);
        }
        if snapshot.ai_panel_open {
            self.render_ai_panel = true;
        }

        let left_target = snapshot
            .left_sidebar
            .map(|sidebar| sidebar.width(snapshot.widths))
            .unwrap_or_default();
        let right_target = if snapshot.ai_panel_open {
            snapshot.widths.ai
        } else {
            0.0
        };
        self.left_width.retarget(left_target, now, reduce_motion);
        self.right_width.retarget(right_target, now, reduce_motion);

        let left = self.left_width.value(now).max(0.0);
        let right = self.right_width.value(now).max(0.0);
        let animating = self.left_width.is_animating(now) || self.right_width.is_animating(now);
        if left <= f32::EPSILON && left_target == 0.0 && !self.left_width.is_animating(now) {
            self.rendered_left_sidebar = None;
        }
        if right <= f32::EPSILON && right_target == 0.0 && !self.right_width.is_animating(now) {
            self.render_ai_panel = false;
        }
        (left, right, animating)
    }
}

fn fit_dock_widths(
    left_preferred: Option<f32>,
    right_preferred: Option<f32>,
    workspace_width: f32,
) -> (Option<f32>, Option<f32>) {
    // Match Zed's fixed dock model while keeping DocumentDockSnapshot aligned
    // with the width actually rendered. These reductions are snapshot-only;
    // passive window resizing never writes them back to preferred_widths.
    let open_count = usize::from(left_preferred.is_some()) + usize::from(right_preferred.is_some());
    let workspace_width = if workspace_width.is_finite() {
        workspace_width.max(0.0)
    } else {
        0.0
    };
    let available = (workspace_width - DOCK_SEPARATOR_WIDTH * open_count as f32).max(0.0);
    let normalize = |width: f32| {
        if width.is_finite() {
            width.max(SIDEBAR_MIN_WIDTH)
        } else {
            SIDEBAR_MIN_WIDTH
        }
    };

    match (
        left_preferred.map(normalize),
        right_preferred.map(normalize),
    ) {
        (None, None) => (None, None),
        (Some(left), None) => (Some(left.min(available)), None),
        (None, Some(right)) => (None, Some(right.min(available))),
        (Some(left), Some(right)) => {
            let preferred_total = left + right;
            if preferred_total <= available {
                return (Some(left), Some(right));
            }

            let minimum_total = SIDEBAR_MIN_WIDTH * 2.0;
            if available <= minimum_total {
                let width = available / 2.0;
                return (Some(width), Some(width));
            }

            let remaining = available - minimum_total;
            let left_headroom = left - SIDEBAR_MIN_WIDTH;
            let right_headroom = right - SIDEBAR_MIN_WIDTH;
            let total_headroom = left_headroom + right_headroom;
            if total_headroom <= f32::EPSILON {
                let width = available / 2.0;
                return (Some(width), Some(width));
            }

            (
                Some(SIDEBAR_MIN_WIDTH + remaining * left_headroom / total_headroom),
                Some(SIDEBAR_MIN_WIDTH + remaining * right_headroom / total_headroom),
            )
        }
    }
}

fn clamp_dragged_dock_width(
    candidate: f32,
    workspace_width: f32,
    opposite_width: f32,
    open_count: usize,
) -> f32 {
    let max_width = (workspace_width - opposite_width - DOCK_SEPARATOR_WIDTH * open_count as f32)
        .max(SIDEBAR_MIN_WIDTH);
    candidate.clamp(SIDEBAR_MIN_WIDTH, max_width)
}

impl Workspace {
    fn schedule_dock_width_save(&mut self, cx: &mut Context<Self>) {
        self.docks.width_save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            _ = this.update(cx, |this, cx| {
                let widths = sanitize_sidebar_widths(this.docks.preferred_widths);
                this.docks.preferred_widths = widths;
                if let Err(error) = cx.global_mut::<LibraryStore>().set_sidebar_widths(widths) {
                    eprintln!("failed to save sidebar widths: {error:#}");
                }
            });
        }));
    }

    fn set_preferred_dock_width(
        &mut self,
        position: WorkspaceDockPosition,
        width: f32,
        cx: &mut Context<Self>,
    ) {
        let width = width.max(SIDEBAR_MIN_WIDTH);
        let current = match position {
            WorkspaceDockPosition::Left => {
                let Some(sidebar) = self.docks.left_sidebar else {
                    return;
                };
                sidebar.width(self.docks.preferred_widths)
            }
            WorkspaceDockPosition::Right => {
                if !self.docks.ai_panel_open {
                    return;
                }
                self.docks.preferred_widths.ai
            }
        };
        if (current - width).abs() <= f32::EPSILON {
            return;
        }

        match position {
            WorkspaceDockPosition::Left => {
                if let Some(sidebar) = self.docks.left_sidebar {
                    sidebar.set_width(&mut self.docks.preferred_widths, width);
                }
                self.docks.left_width.snap(width);
            }
            WorkspaceDockPosition::Right => {
                self.docks.preferred_widths.ai = width;
                self.docks.right_width.snap(width);
            }
        }
        self.schedule_dock_width_save(cx);
        cx.notify();
    }

    fn reset_dock_width(&mut self, position: WorkspaceDockPosition, cx: &mut Context<Self>) {
        let defaults = default_sidebar_widths();
        let width = match position {
            WorkspaceDockPosition::Left => {
                let Some(sidebar) = self.docks.left_sidebar else {
                    return;
                };
                sidebar.width(defaults)
            }
            WorkspaceDockPosition::Right => defaults.ai,
        };
        self.set_preferred_dock_width(position, width, cx);
    }

    fn resize_dock_from_drag(
        &mut self,
        event: &DragMoveEvent<DraggedWorkspaceDock>,
        cx: &mut Context<Self>,
    ) {
        let position = event.drag(cx).0;
        let workspace_width = event.bounds.size.width.as_f32().max(0.0);
        let pointer_x = (event.event.position.x - event.bounds.left())
            .as_f32()
            .clamp(0.0, workspace_width);
        let candidate = match position {
            WorkspaceDockPosition::Left => pointer_x,
            WorkspaceDockPosition::Right => workspace_width - pointer_x,
        };
        let opposite_width = match position {
            WorkspaceDockPosition::Left if self.docks.ai_panel_open => {
                self.docks.preferred_widths.ai
            }
            WorkspaceDockPosition::Right => self
                .docks
                .left_sidebar
                .map(|sidebar| sidebar.width(self.docks.preferred_widths))
                .unwrap_or_default(),
            _ => 0.0,
        };
        let open_count =
            usize::from(self.docks.left_sidebar.is_some()) + usize::from(self.docks.ai_panel_open);
        let width =
            clamp_dragged_dock_width(candidate, workspace_width, opposite_width, open_count);
        self.set_preferred_dock_width(position, width, cx);
    }

    fn render_dock_resize_handle(
        position: WorkspaceDockPosition,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (id, selector) = match position {
            WorkspaceDockPosition::Left => (
                "workspace-left-dock-resize-handle",
                "workspace-left-dock-resize-handle",
            ),
            WorkspaceDockPosition::Right => (
                "workspace-ai-dock-resize-handle",
                "workspace-ai-dock-resize-handle",
            ),
        };
        let handle = div()
            .id(id)
            .debug_selector(move || selector.to_string())
            .absolute()
            .top_0()
            .h_full()
            .w(DOCK_RESIZE_HANDLE_SIZE)
            .cursor_col_resize()
            .occlude()
            .on_drag(DraggedWorkspaceDock(position), |dock, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| dock.clone())
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseUpEvent, _, cx| {
                    if event.click_count >= 2 {
                        this.reset_dock_width(position, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .map(|this| match position {
                WorkspaceDockPosition::Left => this.right(-DOCK_RESIZE_HANDLE_SIZE / 2.0),
                WorkspaceDockPosition::Right => this.left(-DOCK_RESIZE_HANDLE_SIZE / 2.0),
            });

        deferred(handle).into_any_element()
    }

    fn all_document_views(&self, cx: &Context<Self>) -> Vec<Entity<DocumentView>> {
        self.pane_group
            .panes()
            .into_iter()
            .flat_map(|pane| pane.read(cx).document_views())
            .collect()
    }

    fn close_document_sidebar_content(&self, sidebar: DocumentSidebar, cx: &mut Context<Self>) {
        for document_view in self.all_document_views(cx) {
            document_view.update(cx, |document_view, cx| {
                document_view.close_sidebar(sidebar, cx);
            });
        }
    }

    pub(super) fn toggle_left_sidebar(
        &mut self,
        sidebar: DocumentSidebar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous = self.docks.left_sidebar;
        let next = if previous == Some(sidebar) {
            None
        } else {
            Some(sidebar)
        };

        if previous == Some(DocumentSidebar::Thumbnails) && next != previous {
            self.close_document_sidebar_content(DocumentSidebar::Thumbnails, cx);
        }

        self.docks.left_sidebar = next;
        if next.is_none() {
            self.focus_active_pane(window, cx);
        }
        cx.notify();
    }

    pub(super) fn toggle_ai_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.docks.ai_panel_open {
            self.close_ai_dock(cx);
            self.focus_active_pane(window, cx);
            return;
        }

        self.docks.ai_panel_open = true;
        let snapshot = self
            .docks
            .snapshot(window.bounds().size.width.as_f32(), true, true);
        if let Some(document_view) = self.active_pane.read(cx).active_document_view() {
            document_view.update(cx, |document_view, cx| {
                document_view.set_dock_snapshot(snapshot, cx);
                document_view.ensure_ai_panel(snapshot.widths.ai, true, window, cx);
            });
        }
        cx.notify();
    }

    pub(super) fn close_ai_dock(&mut self, cx: &mut Context<Self>) {
        if !self.docks.ai_panel_open {
            return;
        }
        for document_view in self.all_document_views(cx) {
            document_view.update(cx, |document_view, cx| {
                document_view.save_ai_panel_session(cx);
            });
        }
        self.docks.ai_panel_open = false;
        cx.notify();
    }

    fn sync_visible_document_docks(&self, snapshot: DocumentDockSnapshot, cx: &mut Context<Self>) {
        for pane in self.pane_group.panes() {
            if let Some(document_view) = pane.read(cx).active_document_view() {
                document_view.update(cx, |document_view, cx| {
                    document_view.set_dock_snapshot(snapshot, cx);
                });
            }
        }
    }

    fn dock_title(sidebar: DocumentSidebar) -> String {
        match sidebar {
            DocumentSidebar::Thumbnails => crate::fl!("pdf-thumbnails-title"),
            DocumentSidebar::Toc => crate::fl!("table-of-contents-title"),
            DocumentSidebar::Notes => crate::fl!("notes-title"),
        }
    }

    fn render_dock_placeholder(
        sidebar: Option<DocumentSidebar>,
        loading: bool,
        right: bool,
        theme: ThemeColor,
    ) -> AnyElement {
        let title = sidebar
            .map(Self::dock_title)
            .unwrap_or_else(|| crate::fl!("ai-title"));
        let message = if loading {
            crate::fl!("sidebar-content-loading")
        } else {
            crate::fl!("sidebar-content-unavailable")
        };
        let close_id = if right {
            "workspace-ai-sidebar-placeholder-close"
        } else {
            "workspace-left-sidebar-placeholder-close"
        };
        let close = Button::new(close_id)
            .xsmall()
            .compact()
            .ghost()
            .icon(IconName::X)
            .debug_selector(move || close_id.to_string())
            .on_click(move |_, window, cx| match sidebar {
                Some(DocumentSidebar::Thumbnails) => {
                    window.dispatch_action(Box::new(ToggleThumbnailSidebar), cx)
                }
                Some(DocumentSidebar::Toc) => window.dispatch_action(Box::new(ToggleToc), cx),
                Some(DocumentSidebar::Notes) => window.dispatch_action(Box::new(ToggleNotes), cx),
                None => window.dispatch_action(Box::new(ToggleAiPanel), cx),
            });
        let shell = div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(theme.popover)
            .text_size(crate::settings::ui_rems(12.0))
            .text_color(theme.foreground)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_scroll_wheel(|_, _, cx| {
                cx.stop_propagation();
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(40.0))
                    .px_3()
                    .border_b(px(1.0))
                    .border_color(theme.border)
                    .child(title)
                    .child(close),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_3()
                    .text_color(theme.muted_foreground)
                    .child(message),
            );
        if right {
            shell
                .id("workspace-ai-sidebar-placeholder")
                .debug_selector(|| "workspace-ai-sidebar-placeholder".to_string())
                .border_l(px(1.0))
                .border_color(theme.border)
                .into_any_element()
        } else {
            shell
                .id("workspace-left-sidebar-placeholder")
                .debug_selector(|| "workspace-left-sidebar-placeholder".to_string())
                .border_r(px(1.0))
                .border_color(theme.border)
                .into_any_element()
        }
    }

    fn render_workspace_status(theme: ThemeColor, cx: &mut Context<Self>) -> Option<AnyElement> {
        ActivityCenter::try_global(cx)
            .and_then(|center| center.read(cx).render_activity())
            .map(|activity| {
                StatusBar::new()
                    .right(activity)
                    .bg(theme.status_bar)
                    .into_any_element()
            })
    }

    fn render_tool_rail(
        &self,
        document_view: &Entity<DocumentView>,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let supports = |sidebar| document_view.read(cx).supports_sidebar(sidebar);
        let button = |id: &'static str,
                      icon: IconName,
                      sidebar: DocumentSidebar,
                      tooltip: String| {
            Button::new(id)
                .xsmall()
                .compact()
                .ghost()
                .selected(self.docks.left_sidebar == Some(sidebar))
                .icon(icon)
                .w(px(32.0))
                .h(px(32.0))
                .tooltip(tooltip)
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    match sidebar {
                        DocumentSidebar::Thumbnails => {
                            window.dispatch_action(Box::new(ToggleThumbnailSidebar), cx)
                        }
                        DocumentSidebar::Toc => window.dispatch_action(Box::new(ToggleToc), cx),
                        DocumentSidebar::Notes => window.dispatch_action(Box::new(ToggleNotes), cx),
                    }
                })
        };

        div()
            .id("workspace-document-tool-rail")
            .debug_selector(|| "workspace-document-tool-rail".to_string())
            .h_full()
            .w(px(TOOL_RAIL_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .py_2()
            .border_r(px(1.0))
            .border_color(theme.sidebar_border)
            .bg(theme.sidebar)
            .when(supports(DocumentSidebar::Thumbnails), |this| {
                this.child(button(
                    "tool-rail-thumbnails",
                    IconName::PanelLeft,
                    DocumentSidebar::Thumbnails,
                    crate::fl!("pdf-thumbnails-title"),
                ))
            })
            .when(supports(DocumentSidebar::Toc), |this| {
                this.child(button(
                    "tool-rail-toc",
                    IconName::TableOfContents,
                    DocumentSidebar::Toc,
                    crate::fl!("table-of-contents-title"),
                ))
            })
            .when(supports(DocumentSidebar::Notes), |this| {
                this.child(button(
                    "tool-rail-notes",
                    IconName::NotebookTabs,
                    DocumentSidebar::Notes,
                    crate::fl!("notes-title"),
                ))
            })
            .into_any_element()
    }

    pub(super) fn render_body(
        &mut self,
        theme: ThemeColor,
        dim_inactive_panes: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active_document_view = self.active_pane.read(cx).active_document_view();
        if let Some(active_document_view) = active_document_view.clone()
            && active_document_view.read(cx).is_presentation_mode()
        {
            return active_document_view.into_any_element();
        }

        let loading = self.active_pane.read(cx).active_tab_is_loading();
        let docks_visible = active_document_view.is_some() || loading;
        let tool_rail_visible = active_document_view.is_some();
        let mut snapshot = self.docks.snapshot(
            window.bounds().size.width.as_f32(),
            docks_visible,
            tool_rail_visible,
        );
        let now = Instant::now();
        let (left_render_width, right_render_width, animating) =
            self.docks.update_motion(snapshot, now, cx.reduce_motion());
        snapshot.layout_motion_active = animating;
        self.sync_visible_document_docks(snapshot, cx);
        if animating {
            window.request_animation_frame();
        }
        let center =
            self.pane_group
                .render(&self.active_pane, theme, dim_inactive_panes, window, cx);
        let mut main_panels = Vec::with_capacity(3);

        if let Some(document_view) = active_document_view.as_ref() {
            main_panels.push(self.render_tool_rail(document_view, theme, cx));
        }

        if let Some(sidebar) = self.docks.rendered_left_sidebar {
            let content_width = sidebar.width(snapshot.widths).max(SIDEBAR_MIN_WIDTH);
            let content = active_document_view
                .as_ref()
                .filter(|document_view| document_view.read(cx).supports_sidebar(sidebar))
                .map(|document_view| {
                    document_view.update(cx, |document_view, cx| {
                        document_view.render_sidebar(sidebar, theme, window, cx)
                    })
                })
                .unwrap_or_else(|| {
                    Self::render_dock_placeholder(Some(sidebar), loading, false, theme)
                });
            main_panels.push(
                div()
                    .id("workspace-left-dock")
                    .debug_selector(|| "workspace-left-dock".to_string())
                    .relative()
                    .h_full()
                    .w(px(left_render_width))
                    .flex_none()
                    .overflow_hidden()
                    .child(
                        div()
                            .h_full()
                            .w(px(content_width))
                            .flex_none()
                            .child(content),
                    )
                    .when(snapshot.left_sidebar.is_some(), |this| {
                        this.child(Self::render_dock_resize_handle(
                            WorkspaceDockPosition::Left,
                            cx,
                        ))
                    })
                    .into_any_element(),
            );
        }

        main_panels.push(
            div()
                .id("workspace-dock-center")
                .debug_selector(|| "workspace-dock-center".to_string())
                .flex_1()
                .min_w_0()
                .min_h_0()
                .overflow_hidden()
                .child(center)
                .into_any_element(),
        );

        let ai_card = if self.docks.render_ai_panel {
            let content_width = snapshot.widths.ai.max(SIDEBAR_MIN_WIDTH);
            let content = active_document_view
                .as_ref()
                .map(|document_view| {
                    let panel = document_view.update(cx, |document_view, cx| {
                        document_view.ensure_ai_panel(snapshot.widths.ai, false, window, cx)
                    });
                    div().size_full().min_w_0().child(panel).into_any_element()
                })
                .unwrap_or_else(|| Self::render_dock_placeholder(None, loading, true, theme));
            Some(
                div()
                    .id("workspace-ai-dock")
                    .debug_selector(|| "workspace-ai-dock".to_string())
                    .relative()
                    .h_full()
                    .w(px(right_render_width))
                    .flex_none()
                    .overflow_hidden()
                    .flex()
                    .justify_end()
                    .child(
                        div()
                            .h_full()
                            .w(px(content_width))
                            .flex_none()
                            .overflow_hidden()
                            .rounded(cx.theme().radius_lg)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.background)
                            .when(cx.theme().shadow, |this| this.shadow_sm())
                            .child(content),
                    )
                    .when(snapshot.ai_panel_open, |this| {
                        this.child(Self::render_dock_resize_handle(
                            WorkspaceDockPosition::Right,
                            cx,
                        ))
                    })
                    .into_any_element(),
            )
        } else {
            None
        };

        let main_row = div()
            .id("workspace-main-dock-panels")
            .debug_selector(|| "workspace-main-dock-panels".to_string())
            .size_full()
            .flex()
            .children(main_panels);
        let footbar = active_document_view
            .map(|document_view| {
                document_view.update(cx, |document_view, cx| {
                    document_view.render_workspace_footbar(theme, snapshot, cx)
                })
            })
            .or_else(|| Self::render_workspace_status(theme, cx));

        let main_card = div()
            .id("workspace-main-card")
            .debug_selector(|| "workspace-main-card".to_string())
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .when(cx.theme().shadow, |this| this.shadow_sm())
            .child(div().flex_1().min_h_0().child(main_row))
            .when_some(footbar, |this, footbar| this.child(footbar));

        div()
            .id("workspace-dock-panels")
            .debug_selector(|| "workspace-dock-panels".to_string())
            .size_full()
            .flex_1()
            .min_h_0()
            .flex()
            .gap(px(SURFACE_GAP))
            .px(px(SURFACE_INSET))
            .pb(px(SURFACE_INSET))
            .child(main_card)
            .children(ai_card)
            .on_drag_move::<DraggedWorkspaceDock>(cx.listener(
                |this, event: &DragMoveEvent<DraggedWorkspaceDock>, _, cx| {
                    this.resize_dock_from_drag(event, cx);
                },
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{clamp_dragged_dock_width, fit_dock_widths};
    use crate::document_view::SIDEBAR_MIN_WIDTH;

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.01,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn preferred_dock_widths_are_unchanged_when_they_fit() {
        let (left, right) = fit_dock_widths(Some(280.0), Some(360.0), 1_000.0);

        assert_eq!(left, Some(280.0));
        assert_eq!(right, Some(360.0));
    }

    #[test]
    fn constrained_docks_share_only_the_available_width() {
        let (left, right) = fit_dock_widths(Some(280.0), Some(360.0), 500.0);
        let left = left.unwrap();
        let right = right.unwrap();

        assert!(left > SIDEBAR_MIN_WIDTH);
        assert!(right > SIDEBAR_MIN_WIDTH);
        assert_close(left + right, 498.0);
        assert_close(
            (left - SIDEBAR_MIN_WIDTH) / (right - SIDEBAR_MIN_WIDTH),
            130.0 / 210.0,
        );
    }

    #[test]
    fn physically_impossible_minimums_fit_without_overflowing() {
        let (left, right) = fit_dock_widths(Some(280.0), Some(360.0), 250.0);

        assert_eq!(left, Some(124.0));
        assert_eq!(right, Some(124.0));

        let (left, right) = fit_dock_widths(Some(280.0), None, 100.0);
        assert_eq!(left, Some(99.0));
        assert_eq!(right, None);
    }

    #[test]
    fn dragging_is_not_limited_to_half_the_workspace() {
        assert_eq!(clamp_dragged_dock_width(800.0, 1_000.0, 0.0, 1), 800.0);
        assert_eq!(clamp_dragged_dock_width(800.0, 1_000.0, 360.0, 2), 638.0);
        assert_eq!(
            clamp_dragged_dock_width(20.0, 1_000.0, 0.0, 1),
            SIDEBAR_MIN_WIDTH
        );
    }
}
