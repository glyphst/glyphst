use std::time::Duration;

use gpui::{AnyElement, Context, Pixels, Task, Window, deferred, div, prelude::*, px};
use gpui_component::{
    Sizable as _, ThemeColor,
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

const DOCK_RESIZE_HANDLE_SIZE: Pixels = px(6.0);
const DOCK_SEPARATOR_WIDTH: f32 = 1.0;

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
            width_save_task: None,
        }
    }

    pub(super) fn snapshot(&self, workspace_width: f32) -> DocumentDockSnapshot {
        let mut widths = self.preferred_widths;
        let left_preferred = self
            .left_sidebar
            .map(|sidebar| sidebar.width(self.preferred_widths));
        let right_preferred = self.ai_panel_open.then_some(self.preferred_widths.ai);
        let (left_width, right_width) =
            fit_dock_widths(left_preferred, right_preferred, workspace_width);

        if let (Some(sidebar), Some(width)) = (self.left_sidebar, left_width) {
            sidebar.set_width(&mut widths, width);
        }
        if let Some(width) = right_width {
            widths.ai = width;
        }

        DocumentDockSnapshot {
            left_sidebar: self.left_sidebar,
            ai_panel_open: self.ai_panel_open,
            widths,
        }
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
            }
            WorkspaceDockPosition::Right => self.docks.preferred_widths.ai = width,
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
        _window: &mut Window,
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
        cx.notify();
    }

    pub(super) fn toggle_ai_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.docks.ai_panel_open {
            self.close_ai_dock(cx);
            self.focus_active_pane(window, cx);
            return;
        }

        self.docks.ai_panel_open = true;
        let snapshot = self.docks.snapshot(window.bounds().size.width.as_f32());
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

        let snapshot = self.docks.snapshot(window.bounds().size.width.as_f32());
        self.sync_visible_document_docks(snapshot, cx);
        let center =
            self.pane_group
                .render(&self.active_pane, theme, dim_inactive_panes, window, cx);
        let loading = self.active_pane.read(cx).active_tab_is_loading();
        let mut dock_panels = Vec::with_capacity(3);

        if let Some(sidebar) = snapshot.left_sidebar {
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
            dock_panels.push(
                div()
                    .id("workspace-left-dock")
                    .debug_selector(|| "workspace-left-dock".to_string())
                    .relative()
                    .h_full()
                    .w(px(sidebar.width(snapshot.widths)))
                    .flex_none()
                    .child(content)
                    .child(Self::render_dock_resize_handle(
                        WorkspaceDockPosition::Left,
                        cx,
                    ))
                    .into_any_element(),
            );
        }

        dock_panels.push(
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

        if snapshot.ai_panel_open {
            let content = active_document_view
                .as_ref()
                .map(|document_view| {
                    let panel = document_view.update(cx, |document_view, cx| {
                        document_view.ensure_ai_panel(snapshot.widths.ai, false, window, cx)
                    });
                    div().size_full().min_w_0().child(panel).into_any_element()
                })
                .unwrap_or_else(|| Self::render_dock_placeholder(None, loading, true, theme));
            dock_panels.push(
                div()
                    .id("workspace-ai-dock")
                    .debug_selector(|| "workspace-ai-dock".to_string())
                    .relative()
                    .h_full()
                    .w(px(snapshot.widths.ai))
                    .flex_none()
                    .child(content)
                    .child(Self::render_dock_resize_handle(
                        WorkspaceDockPosition::Right,
                        cx,
                    ))
                    .into_any_element(),
            );
        }

        let dock_row = div()
            .id("workspace-dock-panels")
            .debug_selector(|| "workspace-dock-panels".to_string())
            .size_full()
            .flex()
            .children(dock_panels)
            .on_drag_move::<DraggedWorkspaceDock>(cx.listener(
                |this, event: &DragMoveEvent<DraggedWorkspaceDock>, _, cx| {
                    this.resize_dock_from_drag(event, cx);
                },
            ));
        let footbar = active_document_view
            .map(|document_view| {
                document_view.update(cx, |document_view, cx| {
                    document_view.render_workspace_footbar(theme, snapshot, cx)
                })
            })
            .or_else(|| Self::render_workspace_status(theme, cx));

        div()
            .size_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(theme.tab_active)
            .child(div().flex_1().min_h_0().child(dock_row))
            .when_some(footbar, |this, footbar| this.child(footbar))
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
