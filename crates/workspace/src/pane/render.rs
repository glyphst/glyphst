//! How the dock's center looks: pane surfaces, tab bars and drop previews.
//!
//! GPUI Kit's dock draws nothing itself; this skin supplies the workspace's
//! look and routes every interaction back to the [`Workspace`], which owns the
//! state the tabs show (synchronized groups, the context menu, external drops).

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use gpui::{
    AnimationExt as _, AnyElement, AnyView, App, AppContext as _, Bounds, Div, DragMoveEvent,
    EntityId, ExternalPaths, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    ParentElement as _, Pixels, Stateful, StatefulInteractiveElement as _, Styled as _, WeakEntity,
    Window, div, prelude::FluentBuilder as _, px,
};
use gpui_base::{ResizeHandleContext, ResizeHandleState, spring};
use gpui_component::{
    ActiveTheme as _, ElementExt as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{DockAreaRenderer, DragPanel, DropIndicator, TabGroupContext, TabGroupRenderer},
    h_flex,
    tab::{Tab as ComponentTab, TabBar as ComponentTabBar},
};

use super::{
    PANE_TAB_BAR_HEIGHT, PANE_TAB_GAP, PANE_TAB_MAX_WIDTH, PANE_TAB_MIN_WIDTH, PaneId,
    PaneTabBounds, SynchronizedConnectionTabState, TabDragPreview,
};
use crate::{
    external_drop::render_external_document_drop_overlay_for_pane,
    icons::{Icon, IconName},
    settings::{SettingsStore, TabBarVisibility, ui_rems},
    tab_item::TabItem,
    workspace::Workspace,
};

/// Half the gap between two pane surfaces; each pane pads itself by this.
pub(crate) const PANE_SURFACE_MARGIN: f32 = glyphst_ui::style::SURFACE_GAP / 2.0;

/// Painted geometry the workspace reads back: tab extents for the
/// synchronized-tab lines, pane extents for directional focus.
#[derive(Default)]
pub(crate) struct PaneGeometry {
    pub(crate) tabs: HashMap<EntityId, PaneTabBounds>,
    pub(crate) panes: HashMap<PaneId, Bounds<Pixels>>,
}

/// The workspace's dock skin.
pub(crate) struct ReaderDockSkin {
    workspace: WeakEntity<Workspace>,
    geometry: Rc<RefCell<PaneGeometry>>,
}

impl ReaderDockSkin {
    pub(crate) fn new(
        workspace: WeakEntity<Workspace>,
        geometry: Rc<RefCell<PaneGeometry>>,
    ) -> Rc<Self> {
        Rc::new(Self {
            workspace,
            geometry,
        })
    }
}

impl DockAreaRenderer for ReaderDockSkin {
    fn frame(&self, _: &mut Window, _: &mut App) -> Stateful<Div> {
        div().id("workspace-pane-area")
    }

    fn render_split_handle(
        &self,
        handle: &ResizeHandleContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        // The gap between two pane surfaces is the divider. A line shows only
        // while the pointer is on it, so a resting layout reads as separate
        // cards rather than ruled panels.
        let color = match handle.state() {
            ResizeHandleState::Idle => None,
            ResizeHandleState::Hovered => Some(cx.theme().colors.border),
            ResizeHandleState::Pressed | ResizeHandleState::Dragging => {
                Some(cx.theme().colors.drag_border)
            }
        };
        Some(
            div()
                .flex_none()
                .size_full()
                .when_some(color, |this, color| this.bg(color))
                .into_any_element(),
        )
    }

    fn tab_group_renderer(&self) -> Rc<dyn TabGroupRenderer> {
        Rc::new(ReaderTabGroupSkin {
            workspace: self.workspace.clone(),
            geometry: self.geometry.clone(),
        })
    }
}

struct ReaderTabGroupSkin {
    workspace: WeakEntity<Workspace>,
    geometry: Rc<RefCell<PaneGeometry>>,
}

/// What the skin reads from the workspace to draw one group.
struct GroupPresentation {
    show_active_outline: bool,
    dim: bool,
    show_tab_bar: bool,
}

impl ReaderTabGroupSkin {
    fn presentation(&self, group: &TabGroupContext, cx: &App) -> GroupPresentation {
        let pane = PaneId::from(group.node());
        let Some(workspace) = self.workspace.upgrade() else {
            return GroupPresentation {
                show_active_outline: false,
                dim: false,
                show_tab_bar: true,
            };
        };
        let workspace = workspace.read(cx);
        let settings = &cx.global::<SettingsStore>().settings;
        let split = workspace.pane_count(cx) > 1;
        let active = workspace.active_pane_id() == pane;
        GroupPresentation {
            show_active_outline: split && active,
            dim: settings.behavior.dim_inactive_panes && !active,
            show_tab_bar: settings.tab_bar_visibility == TabBarVisibility::Always
                || split
                || group.panels().len() >= 2,
        }
    }

    fn tabs(group: &TabGroupContext) -> Vec<gpui::Entity<TabItem>> {
        group
            .panels()
            .iter()
            .filter_map(|panel| {
                panel
                    .as_any()
                    .downcast_ref::<gpui::Entity<TabItem>>()
                    .cloned()
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_tab(
        &self,
        group: &TabGroupContext,
        workspace: &gpui::Entity<Workspace>,
        tabs: &[gpui::Entity<TabItem>],
        tab_ix: usize,
        theme: gpui_component::ThemeColor,
        cx: &mut App,
    ) -> ComponentTab {
        let pane = PaneId::from(group.node());
        let tab = &tabs[tab_ix];
        let tab_id = tab.entity_id();
        let label = tab.read(cx).label(cx);
        let draggable = group.is_draggable() && tab.read(cx).is_draggable(cx);
        let presentation = workspace.read(cx).synchronized_presentation().clone();
        let group_summary = presentation.group_by_tab.get(&tab_id).cloned();
        let is_paused = presentation.paused_tabs.contains(&tab_id);
        let synchronized_theme = cx.global::<SettingsStore>().resolved_theme;
        let current_color = group_summary
            .as_ref()
            .filter(|group| group.is_current)
            .map(|group| crate::synchronized_group_color(synchronized_theme, group.color_slot));
        let connection_color = presentation
            .connection_color_slot
            .map(|color_slot| crate::synchronized_group_color(synchronized_theme, color_slot));
        let recently_added = presentation.recently_added;
        let connection_state = presentation.connection_tabs.get(&tab_id).copied();
        let occurrence = workspace
            .read(cx)
            .pane(pane, cx)
            .document_tab_occurrence(tab_ix, cx);
        let accessible_label = occurrence.map(|(ordinal, total)| {
            crate::fl!(
                "tab-document-view-label",
                name = label.to_string(),
                index = ordinal as i64,
                count = total as i64
            )
        });
        let close_workspace = workspace.downgrade();
        let close_button = Button::new(("pane-tab-close", tab_id))
            .small()
            .compact()
            .ghost()
            .icon(IconName::X)
            .tooltip(crate::fl!("toolbar-close-tab"))
            .debug_selector(move || format!("pane-tab-close-{tab_ix}"))
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                _ = close_workspace.update(cx, |workspace, cx| {
                    workspace.close_tab_by_id(tab_id, window, cx);
                });
            });

        let group_marker = group_summary.map(|group| {
            let color = crate::synchronized_group_color(synchronized_theme, group.color_slot);
            let marker_width = 16.0;
            let marker_height = 20.0;
            let marker = div()
                .id(("synchronized-tab-group-link", tab_id))
                .w(px(marker_width))
                .h(px(marker_height))
                .flex()
                .items_center()
                .justify_center()
                .debug_selector(|| "synchronized-tab-group-link".to_string())
                .tooltip(glyphst_ui::tooltip::text_tooltip(crate::fl!(
                    "synchronized-tabs-group-summary",
                    number = group.display_number,
                    count = group.member_count
                )))
                .child(
                    Icon::new(IconName::Link2)
                        .size(ui_rems(12.0))
                        .text_color(color),
                );
            let is_recently_added = recently_added
                .is_some_and(|recent| recent.tab_id == tab_id && recent.group_id == group.id);
            if is_recently_added {
                let sequence = recently_added.map_or(0, |recent| recent.sequence);
                marker
                    .with_animation(
                        ("synchronized-tab-added", sequence as usize),
                        glyphst_ui::motion::HOVER.animation(),
                        move |element, progress| {
                            let scale = if progress < 0.6 {
                                0.6 + progress / 0.6 * 0.55
                            } else {
                                1.15 - (progress - 0.6) / 0.4 * 0.15
                            };
                            element
                                .w(px(marker_width * scale))
                                .h(px(marker_height * scale))
                                .opacity(progress)
                        },
                    )
                    .into_any_element()
            } else {
                marker.into_any_element()
            }
        });
        let group_marker = h_flex()
            .children(group_marker)
            .when(is_paused, |this| this.opacity(0.45));
        let suffix_geometry = self.geometry.clone();
        let suffix = h_flex()
            .flex_none()
            .gap_1()
            .children(occurrence.map(|(ordinal, total)| {
                div()
                    .id(("pane-tab-occurrence", tab_id))
                    .min_w(px(16.0))
                    .h(px(16.0))
                    .px_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(theme.muted)
                    .text_size(ui_rems(10.0))
                    .text_color(theme.muted_foreground)
                    .debug_selector(move || format!("pane-tab-copy-index-{tab_ix}"))
                    .tooltip(glyphst_ui::tooltip::text_tooltip(crate::fl!(
                        "tab-document-view-label",
                        name = label.to_string(),
                        index = ordinal as i64,
                        count = total as i64
                    )))
                    .child(ordinal.to_string())
            }))
            .child(group_marker)
            .when(is_paused, |this| {
                this.child(
                    div()
                        .w(px(12.0))
                        .h(px(12.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .debug_selector(|| "synchronized-tab-paused".to_string())
                        .child(
                            Icon::new(IconName::Pause)
                                .size(ui_rems(10.0))
                                .text_color(theme.muted_foreground),
                        ),
                )
            })
            .child(close_button)
            .on_prepaint(move |bounds, _, _| {
                suffix_geometry
                    .borrow_mut()
                    .tabs
                    .entry(tab_id)
                    .or_default()
                    .suffix = Some(bounds);
            });
        let body_geometry = self.geometry.clone();
        let menu_workspace = workspace.downgrade();
        let drag = draggable
            .then(|| group.drag_panel(tab_ix, cx))
            .flatten()
            .filter(|_| connection_state.is_none());
        let droppable = group.is_droppable();

        ComponentTab::new()
            .label(label.clone())
            .when_some(accessible_label, |this, label| this.aria_label(label))
            .suffix(suffix)
            .rounded(cx.theme().radius)
            .max_w(ui_rems(PANE_TAB_MAX_WIDTH))
            .min_w(ui_rems(PANE_TAB_MIN_WIDTH))
            .when(tab_ix > 0, |this| this.ml(px(PANE_TAB_GAP)))
            .debug_selector(move || format!("pane-tab-{tab_ix}"))
            .on_prepaint(move |bounds, _, _| {
                body_geometry
                    .borrow_mut()
                    .tabs
                    .entry(tab_id)
                    .or_default()
                    .body = Some(bounds);
            })
            .on_mouse_down(
                MouseButton::Right,
                move |event: &MouseDownEvent, window, cx| {
                    _ = menu_workspace.update(cx, |workspace, cx| {
                        workspace.open_tab_context_menu(pane, tab_id, event.position, window, cx);
                    });
                },
            )
            .when_some(current_color, |this, color| {
                this.border_b(px(2.0)).border_color(color)
            })
            .when_some(connection_state, |this, state| match state {
                SynchronizedConnectionTabState::Source | SynchronizedConnectionTabState::Member => {
                    this.when_some(connection_color, |this, color| {
                        this.border_b(px(2.0)).border_color(color)
                    })
                }
                SynchronizedConnectionTabState::Eligible => {
                    let color = connection_color.unwrap_or(theme.primary);
                    let hover_group = format!("synchronized-candidate-{}", tab_id.as_u64());
                    let highlight = div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .left_0()
                        .rounded_sm()
                        .group_hover(hover_group.clone(), move |style| {
                            style.bg(color.opacity(0.12)).border_2().border_color(color)
                        });
                    this.opacity(0.72)
                        .cursor_pointer()
                        .group(hover_group)
                        .prefix(highlight)
                }
                SynchronizedConnectionTabState::Invalid(_) => {
                    this.opacity(0.25).cursor_not_allowed()
                }
            })
            .when_some(drag, |this, drag| {
                let label = label.clone();
                this.on_drag(drag, move |drag, offset, _, cx| {
                    cx.stop_propagation();
                    drag.set_drag_offset(offset);
                    cx.new(|_| TabDragPreview {
                        label: label.clone(),
                    })
                })
            })
            .when(droppable && connection_state.is_none(), |this| {
                let node = group.node();
                let group = group.clone();
                let panel_ids = tabs
                    .iter()
                    .map(|tab| super::panel_id(tab.entity_id()))
                    .collect::<Vec<_>>();
                this.drag_over::<DragPanel>(move |style, drag, _, _| {
                    let style = style.bg(theme.list_hover);
                    let dragged_ix = (drag.source() == node)
                        .then(|| panel_ids.iter().position(|panel| *panel == drag.panel()))
                        .flatten();
                    match dragged_ix {
                        Some(dragged_ix) if tab_ix > dragged_ix => {
                            style.border_r(px(2.0)).border_color(theme.primary)
                        }
                        Some(dragged_ix) if tab_ix == dragged_ix => style,
                        _ => style.border_l(px(2.0)).border_color(theme.primary),
                    }
                })
                .on_drop(move |drag: &DragPanel, window, cx| {
                    group.drop_panel(drag.clone(), Some(tab_ix), true, window, cx);
                })
            })
    }

    fn render_tabs(
        &self,
        group: &TabGroupContext,
        workspace: &gpui::Entity<Workspace>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let _ = window;
        let theme = cx.theme().colors;
        let pane = PaneId::from(group.node());
        let tabs = Self::tabs(group);
        let rendered = (0..tabs.len())
            .map(|tab_ix| self.render_tab(group, workspace, &tabs, tab_ix, theme, cx))
            .collect::<Vec<_>>();

        let new_tab_workspace = workspace.downgrade();
        let new_tab_button = Button::new(("pane-new-tab", pane.as_u64()))
            .small()
            .compact()
            .ghost()
            .icon(IconName::Plus)
            .tooltip(crate::fl!("toolbar-new-tab"))
            .w(ui_rems(32.0))
            .h_full()
            .debug_selector(|| "pane-new-tab".to_string())
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                _ = new_tab_workspace.update(cx, |workspace, cx| {
                    workspace.open_new_tab_in_pane(pane, window, cx);
                });
            });

        let droppable = group.is_droppable();
        let tab_count = tabs.len();
        let append_group = group.clone();
        let append_target = div()
            .id(("pane-tab-append", pane.as_u64()))
            .flex_1()
            .min_w(ui_rems(24.0))
            .h_full()
            .when(droppable, |this| {
                this.drag_over::<DragPanel>(move |style, _, _, _| {
                    style
                        .bg(theme.list_hover)
                        .border_l(px(2.0))
                        .border_color(theme.primary)
                })
                .on_drop(move |drag: &DragPanel, window, cx| {
                    // A tab dropped past its own pane's last tab moves to the
                    // end; one from elsewhere is appended and shown.
                    let ix =
                        (drag.source() == append_group.node()).then(|| tab_count.saturating_sub(1));
                    append_group.drop_panel(drag.clone(), ix, true, window, cx);
                })
            });

        let click_workspace = workspace.downgrade();
        let selected = group.active_ix();
        let tab_ids = tabs.iter().map(|tab| tab.entity_id()).collect::<Vec<_>>();
        ComponentTabBar::new(("pane-tab-bar", pane.as_u64()))
            .flex_none()
            .w_full()
            .h(ui_rems(PANE_TAB_BAR_HEIGHT))
            .px(px(2.0))
            .selected_index(selected)
            .menu(true)
            .children(rendered)
            .last_empty_space(append_target)
            .suffix(new_tab_button)
            .on_click(move |tab_ix, window, cx| {
                let Some(tab_id) = tab_ids.get(*tab_ix).copied() else {
                    return;
                };
                _ = click_workspace.update(cx, |workspace, cx| {
                    workspace.click_tab(pane, tab_id, window, cx);
                });
            })
            .into_any_element()
    }
}

impl TabGroupRenderer for ReaderTabGroupSkin {
    fn frame(&self, group: &TabGroupContext, _: &mut Window, cx: &mut App) -> Stateful<Div> {
        let presentation = self.presentation(group, cx);
        let theme = cx.theme().colors;
        let radius = cx.theme().radius;
        let pane = PaneId::from(group.node());
        let pane_id = pane.as_u64();
        // The surface is drawn behind the tab bar and content, inset by the
        // margin that makes the gap between neighbouring panes.
        let surface_geometry = self.geometry.clone();
        let surface = div()
            .absolute()
            .top(px(PANE_SURFACE_MARGIN))
            .right(px(PANE_SURFACE_MARGIN))
            .bottom(px(PANE_SURFACE_MARGIN))
            .left(px(PANE_SURFACE_MARGIN))
            .rounded(radius)
            .border_1()
            .border_color(if presentation.show_active_outline {
                theme.primary
            } else {
                theme.border
            })
            .bg(theme.background)
            .debug_selector(move || format!("pane-surface-{pane_id}"))
            .when(presentation.show_active_outline, |this| {
                this.debug_selector(|| "active-pane-surface".to_string())
            })
            .on_prepaint(move |bounds, _, _| {
                // Measured inside the border; the surface includes it.
                surface_geometry
                    .borrow_mut()
                    .panes
                    .insert(pane, bounds.dilate(px(1.0)));
            });
        div()
            .id(("workspace-pane", pane_id))
            .relative()
            .p(px(PANE_SURFACE_MARGIN + 1.0))
            .child(surface)
    }

    fn content_frame(
        &self,
        group: &TabGroupContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        let pane = PaneId::from(group.node());
        let workspace = self.workspace.clone();
        let drop_workspace = self.workspace.clone();
        let move_workspace = self.workspace.clone();
        let _ = cx;
        div()
            .id(("workspace-pane-content", pane.as_u64()))
            .relative()
            .capture_any_mouse_down(move |_: &MouseDownEvent, window, cx| {
                _ = workspace.update(cx, |workspace, cx| {
                    workspace.activate_pane_from_click(pane, window, cx);
                });
            })
            .on_drag_move::<ExternalPaths>(move |event: &DragMoveEvent<ExternalPaths>, _, cx| {
                _ = move_workspace.update(cx, |workspace, cx| {
                    workspace.track_pane_external_drag(pane, event, cx);
                });
            })
            .on_drop(move |paths: &ExternalPaths, window, cx| {
                cx.stop_propagation();
                _ = drop_workspace.update(cx, |workspace, cx| {
                    workspace.drop_external_paths_on_pane(pane, paths.paths(), window, cx);
                });
            })
    }

    fn render_tab_bar(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let Some(workspace) = self.workspace.upgrade() else {
            return gpui::Empty.into_any_element();
        };
        if !self.presentation(group, cx).show_tab_bar {
            return gpui::Empty.into_any_element();
        }
        self.render_tabs(group, &workspace, window, cx)
    }

    fn render_active_panel(
        &self,
        panel: AnyView,
        group: &TabGroupContext,
        _: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let presentation = self.presentation(group, cx);
        let theme = cx.theme().colors;
        let pane = PaneId::from(group.node());
        let external_drop = cx
            .has_active_drag()
            .then(|| self.workspace.upgrade())
            .flatten()
            .and_then(|workspace| workspace.read(cx).pane_external_drop_feedback(pane));
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .child(panel)
            .when(presentation.dim, |this| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .left_0()
                        .bg(theme.overlay),
                )
            })
            .children(external_drop.map(|feedback| {
                render_external_document_drop_overlay_for_pane(feedback, theme, pane.as_u64())
            }))
            .into_any_element()
    }

    fn render_drop_indicator(
        &self,
        indicator: DropIndicator,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let theme = cx.theme().colors;
        let to = indicator.to();
        let id = "pane-drop-preview";
        let policy = cx.theme().motion_tokens().spring_move.with_epsilon(0.5);
        let left = spring((id, "left"), to.origin().x, policy, window, cx);
        let top = spring((id, "top"), to.origin().y, policy, window, cx);
        let width = spring((id, "width"), to.size().width, policy, window, cx);
        let height = spring((id, "height"), to.size().height, policy, window, cx);
        Some(
            div()
                .absolute()
                .left(left)
                .top(top)
                .w(width)
                .h(height)
                .bg(theme.overlay)
                .border_2()
                .border_color(theme.primary.opacity(0.6))
                .rounded(cx.theme().radius)
                .debug_selector(|| "pane-drop-preview".to_string())
                .into_any_element(),
        )
    }
}
