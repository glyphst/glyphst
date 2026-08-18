use gpui::{
    Anchor, AnyElement, Entity, FocusHandle, Hsla, IntoElement, MouseButton, RenderOnce,
    SharedString, Styled as _, Window, anchored, deferred, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _, TitleBar,
    button::{Button, ButtonVariants as _},
    menu::{PopupMenu, PopupMenuItem},
};

use crate::{
    document_view::{CloseTab, DuplicateTab, OpenFile, OpenNewTab, OpenSettings},
    icons::IconName,
    pane::{SplitDown, SplitRight},
};
use glyphst_ui::activity::ActivityCenter;

use super::Workspace;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkspaceMenu {
    Glyphst,
    File,
    Tabs,
}

fn application_menu(menu: PopupMenu) -> PopupMenu {
    menu.menu_with_icon(
        crate::fl!("toolbar-open-settings"),
        IconName::Settings,
        Box::new(OpenSettings),
    )
    .separator()
    .item(PopupMenuItem::new(crate::fl!("toolbar-quit-glyphst")).on_click(|_, _, cx| cx.quit()))
}

fn file_menu(menu: PopupMenu) -> PopupMenu {
    menu.menu_with_icon(
        crate::fl!("toolbar-open-file"),
        IconName::File,
        Box::new(OpenFile),
    )
}

fn tab_menu(menu: PopupMenu) -> PopupMenu {
    menu.menu_with_icon(
        crate::fl!("toolbar-new-tab"),
        IconName::Plus,
        Box::new(OpenNewTab),
    )
    .menu_with_icon(
        crate::fl!("toolbar-duplicate-tab"),
        IconName::Copy,
        Box::new(DuplicateTab),
    )
    .menu_with_icon(
        crate::fl!("toolbar-close-tab"),
        IconName::X,
        Box::new(CloseTab),
    )
    .separator()
    .menu(crate::fl!("toolbar-split-right"), Box::new(SplitRight))
    .menu(crate::fl!("toolbar-split-down"), Box::new(SplitDown))
}

pub(super) fn build_popup_menu(
    menu: WorkspaceMenu,
    action_context: FocusHandle,
    window: &mut Window,
    cx: &mut gpui::App,
) -> Entity<PopupMenu> {
    PopupMenu::build(window, cx, move |popup, _, _| {
        let popup = popup.action_context(action_context);
        match menu {
            WorkspaceMenu::Glyphst => application_menu(popup),
            WorkspaceMenu::File => file_menu(popup),
            WorkspaceMenu::Tabs => tab_menu(popup),
        }
    })
}

#[derive(IntoElement)]
pub(super) struct WorkspaceTitleBar {
    workspace: Entity<Workspace>,
    open_menu: Option<WorkspaceMenu>,
    popup_menu: Option<Entity<PopupMenu>>,
    active_tab_label: Option<SharedString>,
    home_active: bool,
    shell_background: Hsla,
    window_close_handler: Option<fn(&Entity<Workspace>, &mut Window, &mut gpui::App)>,
}

impl WorkspaceTitleBar {
    pub(super) fn new(
        workspace: Entity<Workspace>,
        open_menu: Option<WorkspaceMenu>,
        popup_menu: Option<Entity<PopupMenu>>,
        active_tab_label: Option<SharedString>,
        home_active: bool,
        shell_background: Hsla,
        window_close_handler: Option<fn(&Entity<Workspace>, &mut Window, &mut gpui::App)>,
    ) -> Self {
        Self {
            workspace,
            open_menu,
            popup_menu,
            active_tab_label,
            home_active,
            shell_background,
            window_close_handler,
        }
    }

    fn menu_button(
        id: &'static str,
        label: SharedString,
        icon: IconName,
        menu: WorkspaceMenu,
        workspace: Entity<Workspace>,
        active: bool,
        popup_menu: Option<Entity<PopupMenu>>,
    ) -> AnyElement {
        let click_workspace = workspace.clone();
        div()
            .id(format!("{id}-container"))
            .relative()
            .on_hover(move |hovered, window, cx| {
                if *hovered {
                    workspace.update(cx, |workspace, cx| {
                        workspace.hover_title_bar_menu(menu, window, cx);
                    });
                }
            })
            .child(
                Button::new(id)
                    .small()
                    .compact()
                    .ghost()
                    .icon(icon)
                    .tooltip(label)
                    .selected(active)
                    .debug_selector(move || id.to_string())
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        click_workspace.update(cx, |workspace, cx| {
                            workspace.toggle_title_bar_menu(menu, window, cx);
                        });
                    }),
            )
            .when_some(popup_menu, |this, popup_menu| {
                this.child(deferred(
                    anchored()
                        .anchor(Anchor::TopLeft)
                        .snap_to_window_with_margin(px(8.0))
                        .child(div().size_full().top_1().occlude().child(popup_menu)),
                ))
            })
            .into_any_element()
    }
}

impl RenderOnce for WorkspaceTitleBar {
    fn render(self, _: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let workspace = self.workspace;
        let shell_background = self.shell_background;
        let popup_for = |menu| {
            (self.open_menu == Some(menu))
                .then(|| self.popup_menu.clone())
                .flatten()
        };
        let controls = div()
            .h_full()
            .flex()
            .min_w_0()
            .items_center()
            .gap_1()
            .children([
                Button::new("workspace-home")
                    .small()
                    .compact()
                    .ghost()
                    .icon(IconName::Home)
                    .tooltip("Toggle Home")
                    .selected(self.home_active)
                    .debug_selector(|| "workspace-home".to_string())
                    .on_click({
                        let workspace = workspace.clone();
                        move |_, window, cx| {
                            cx.stop_propagation();
                            workspace.update(cx, |workspace, cx| {
                                workspace.toggle_home(window, cx);
                            });
                        }
                    })
                    .into_any_element(),
                Self::menu_button(
                    "workspace-menu-glyphst",
                    crate::fl!("toolbar-menu-glyphst").into(),
                    IconName::Menu,
                    WorkspaceMenu::Glyphst,
                    workspace.clone(),
                    self.open_menu == Some(WorkspaceMenu::Glyphst),
                    popup_for(WorkspaceMenu::Glyphst),
                ),
                Self::menu_button(
                    "workspace-menu-file",
                    crate::fl!("toolbar-menu-file").into(),
                    IconName::FolderOpen,
                    WorkspaceMenu::File,
                    workspace.clone(),
                    self.open_menu == Some(WorkspaceMenu::File),
                    popup_for(WorkspaceMenu::File),
                ),
                Self::menu_button(
                    "workspace-menu-tab",
                    crate::fl!("toolbar-menu-tab").into(),
                    IconName::Plus,
                    WorkspaceMenu::Tabs,
                    workspace.clone(),
                    self.open_menu == Some(WorkspaceMenu::Tabs),
                    popup_for(WorkspaceMenu::Tabs),
                ),
            ])
            .when_some(self.active_tab_label, |this, label| {
                this.child(
                    div()
                        .min_w_0()
                        .max_w(px(360.0))
                        .truncate()
                        .px_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
            })
            .into_any_element();
        let activity = ActivityCenter::try_global(cx)
            .and_then(|center| center.read(cx).render_activity(Anchor::TopRight));
        let close_workspace = workspace.clone();
        let collapse_workspace = workspace;

        TitleBar::new()
            .border_b(px(0.0))
            .bg(shell_background)
            .child(
                div()
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .child(controls)
                    .child(
                        div()
                            .h_full()
                            .flex_1()
                            .debug_selector(|| "workspace-title-bar-drag-region".to_string())
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                collapse_workspace.update(cx, |workspace, cx| {
                                    workspace.set_title_bar_menu_expanded(false, cx);
                                });
                            }),
                    )
                    .children(activity),
            )
            .when_some(self.window_close_handler, |title_bar, handler| {
                title_bar.on_close_window(move |_, window, cx| {
                    handler(&close_workspace, window, cx);
                })
            })
    }
}
