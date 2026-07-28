use std::time::Duration;

use gpui::{AnyElement, Context, Entity, Pixels, Task, Window, div, prelude::*, px};
use gpui_component::{
    Sizable as _, ThemeColor,
    button::{Button, ButtonVariants as _},
    resizable::{ResizablePanelEvent, ResizablePanelGroup, ResizableState, resizable_panel},
    status_bar::StatusBar,
};

use super::*;
use crate::{
    document_view::{
        DocumentDockSnapshot, DocumentSidebar, SIDEBAR_MAX_WIDTH_RATIO, SIDEBAR_MIN_WIDTH,
        ToggleAiPanel, ToggleNotes, ToggleThumbnailSidebar, ToggleToc, clamp_sidebar_width,
        default_sidebar_widths, sanitize_sidebar_widths,
    },
    icons::IconName,
    library::{LibraryStore, SidebarWidths},
};

pub(super) struct WorkspaceDockState {
    pub(super) left_sidebar: Option<DocumentSidebar>,
    pub(super) ai_panel_open: bool,
    pub(super) widths: SidebarWidths,
    pub(super) resizable_state: Entity<ResizableState>,
    resizable_left_sidebar: Option<DocumentSidebar>,
    resizable_ai_panel_open: bool,
    width_save_task: Option<Task<()>>,
}

impl WorkspaceDockState {
    pub(super) fn new(cx: &mut Context<Workspace>) -> Self {
        let widths = cx
            .global::<LibraryStore>()
            .sidebar_widths()
            .map(sanitize_sidebar_widths)
            .unwrap_or_else(default_sidebar_widths);
        let resizable_state = cx.new(|_| ResizableState::default());
        resizable_state.update(cx, |state, cx| state.insert_panel(None, None, cx));

        Self {
            left_sidebar: None,
            ai_panel_open: false,
            widths,
            resizable_state,
            resizable_left_sidebar: None,
            resizable_ai_panel_open: false,
            width_save_task: None,
        }
    }

    pub(super) fn snapshot(&self) -> DocumentDockSnapshot {
        DocumentDockSnapshot {
            left_sidebar: self.left_sidebar,
            ai_panel_open: self.ai_panel_open,
            widths: self.widths,
        }
    }
}

impl Workspace {
    pub(super) fn observe_dock_resizing(&self, cx: &mut Context<Self>) {
        let state = self.docks.resizable_state.clone();
        cx.subscribe(&state, |this, state, _: &ResizablePanelEvent, cx| {
            let sizes = state.read(cx).sizes().clone();
            this.apply_resized_dock_widths(&sizes, cx);
        })
        .detach();
    }

    fn sync_dock_resizable_state(&mut self, cx: &mut Context<Self>) -> Entity<ResizableState> {
        let state = self.docks.resizable_state.clone();
        let left_sidebar = self.docks.left_sidebar;
        if self.docks.resizable_left_sidebar != left_sidebar {
            let replacement_width =
                left_sidebar.map(|sidebar| px(sidebar.width(self.docks.widths)));
            state.update(cx, |state, cx| {
                match (self.docks.resizable_left_sidebar, replacement_width) {
                    (None, Some(width)) => state.insert_panel(Some(width), Some(0), cx),
                    (Some(_), None) => state.remove_panel(0, cx),
                    (Some(_), Some(width)) => {
                        state.remove_panel(0, cx);
                        state.insert_panel(Some(width), Some(0), cx);
                    }
                    (None, None) => {}
                }
            });
            self.docks.resizable_left_sidebar = left_sidebar;
        }

        if self.docks.resizable_ai_panel_open != self.docks.ai_panel_open {
            state.update(cx, |state, cx| {
                if self.docks.ai_panel_open {
                    state.insert_panel(Some(px(self.docks.widths.ai)), None, cx);
                } else if let Some(last_ix) = state.sizes().len().checked_sub(1) {
                    state.remove_panel(last_ix, cx);
                }
            });
            self.docks.resizable_ai_panel_open = self.docks.ai_panel_open;
        }

        state
    }

    fn apply_resized_dock_widths(&mut self, sizes: &[Pixels], cx: &mut Context<Self>) {
        let expected_len = 1
            + usize::from(self.docks.left_sidebar.is_some())
            + usize::from(self.docks.ai_panel_open);
        if sizes.len() != expected_len {
            return;
        }

        let mut panel_ix = 0;
        let mut changed = false;
        if let Some(sidebar) = self.docks.left_sidebar {
            let width = sizes[panel_ix].as_f32();
            changed |= (sidebar.width(self.docks.widths) - width).abs() > f32::EPSILON;
            sidebar.set_width(&mut self.docks.widths, width);
            panel_ix += 1;
        }

        panel_ix += 1;
        if self.docks.ai_panel_open {
            let width = sizes[panel_ix].as_f32();
            changed |= (self.docks.widths.ai - width).abs() > f32::EPSILON;
            self.docks.widths.ai = width;
        }

        if changed {
            self.schedule_dock_width_save(cx);
            cx.notify();
        }
    }

    fn schedule_dock_width_save(&mut self, cx: &mut Context<Self>) {
        self.docks.width_save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            _ = this.update(cx, |this, cx| {
                let widths = sanitize_sidebar_widths(this.docks.widths);
                this.docks.widths = widths;
                if let Err(error) = cx.global_mut::<LibraryStore>().set_sidebar_widths(widths) {
                    eprintln!("failed to save sidebar widths: {error:#}");
                }
            });
        }));
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
        if let Some(sidebar) = next {
            let width =
                clamp_sidebar_width(Some(sidebar), sidebar.width(self.docks.widths), window);
            sidebar.set_width(&mut self.docks.widths, width);
        }
        cx.notify();
    }

    pub(super) fn toggle_ai_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.docks.ai_panel_open {
            self.close_ai_dock(cx);
            self.focus_active_pane(window, cx);
            return;
        }

        self.docks.widths.ai = clamp_sidebar_width(None, self.docks.widths.ai, window);
        self.docks.ai_panel_open = true;
        let snapshot = self.docks.snapshot();
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

        let snapshot = self.docks.snapshot();
        self.sync_visible_document_docks(snapshot, cx);
        let center =
            self.pane_group
                .render(&self.active_pane, theme, dim_inactive_panes, window, cx);
        let loading = self.active_pane.read(cx).active_tab_is_loading();
        let resizable_state = self.sync_dock_resizable_state(cx);
        let max_sidebar_width =
            (window.bounds().size.width.as_f32() * SIDEBAR_MAX_WIDTH_RATIO).max(SIDEBAR_MIN_WIDTH);
        let sidebar_size_range = px(SIDEBAR_MIN_WIDTH)..px(max_sidebar_width);
        let mut panels = Vec::with_capacity(3);

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
            panels.push(
                resizable_panel()
                    .size(px(sidebar.width(snapshot.widths)))
                    .size_range(sidebar_size_range.clone())
                    .flex_none()
                    .child(content),
            );
        }

        panels.push(
            resizable_panel().size_range(px(100.0)..Pixels::MAX).child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .child(center),
            ),
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
            panels.push(
                resizable_panel()
                    .size(px(snapshot.widths.ai))
                    .size_range(sidebar_size_range)
                    .flex_none()
                    .child(content),
            );
        }

        let resizable = ResizablePanelGroup::new("workspace-dock-panels")
            .with_state(&resizable_state)
            .children(panels);
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
            .child(div().flex_1().min_h_0().child(resizable))
            .when_some(footbar, |this, footbar| this.child(footbar))
            .into_any_element()
    }
}
