use super::*;
use glyphst_actions::workspace::ToggleZenMode;

impl DocumentView {
    pub(super) fn open_context_menu(
        &mut self,
        position: GpuiPoint<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        self.dragging = false;
        self.pending_selection_anchor = None;
        self.panning = None;
        self.context_menu_region =
            !self.grid_overview_open && self.point_in_selected_region(position);
        self.context_menu_image = if self.grid_overview_open
            || self.context_menu_region
            || self.context_menu_highlight.is_some()
            || self.context_menu_note.is_some()
        {
            None
        } else {
            self.image_at_screen(position, cx)
        };
        self.context_menu_position = Some(position);

        let state = ContextMenuState {
            has_page: self.capabilities.page_export
                && !self.context_menu_region
                && self.context_menu_page.is_some(),
            has_image: !self.grid_overview_open && self.context_menu_image.is_some(),
            has_selection: !self.grid_overview_open && self.selected_text().is_some(),
            highlight_color: if self.grid_overview_open {
                None
            } else {
                self.context_menu_highlight
                    .and_then(|id| self.highlight_color_for_id(id))
            },
            has_note: !self.grid_overview_open && self.context_menu_note.is_some(),
            has_region: self.context_menu_region,
            grid_overview: self.grid_overview_open,
        };
        let document_theme = cx.global::<SettingsStore>().resolved_theme;
        let view = cx.entity();
        let action_context = self.focus_handle.clone();
        let popup = PopupMenu::build(window, cx, move |menu, window, menu_cx| {
            Self::build_context_menu_popup(
                menu.action_context(action_context.clone()),
                state,
                window.is_fullscreen(),
                document_theme,
                view.clone(),
                window,
                menu_cx,
            )
        });
        cx.subscribe_in(&popup, window, |this, popup, _: &DismissEvent, _, cx| {
            if this.context_menu_popup.as_ref() == Some(popup) {
                this.close_context_menu(cx);
            }
        })
        .detach();
        self.context_menu_popup = Some(popup.clone());
        popup.focus_handle(cx).focus(window, cx);
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn close_context_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let was_open = self.context_menu_position.take().is_some();
        self.context_menu_popup = None;
        self.context_menu_page = None;
        self.context_menu_image = None;
        self.context_menu_highlight = None;
        self.context_menu_region = false;
        if was_open {
            self.context_menu_note = None;
            cx.notify();
        }
        was_open
    }

    fn context_menu_item(
        label: String,
        view: Entity<Self>,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> PopupMenuItem {
        PopupMenuItem::new(label).on_click(move |_, window, cx| {
            view.update(cx, |this, cx| {
                handler(this, window, cx);
                this.close_context_menu(cx);
            });
        })
    }

    pub(super) fn grid_overview_toolbar_button(
        id: &'static str,
        label: String,
        handler: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Button::new(id)
            .small()
            .outline()
            .label(label)
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                handler(this, event, window, cx);
                cx.stop_propagation();
            }))
    }

    pub(super) fn page_selection_actions_button(
        id: &'static str,
        label: String,
        source: PageSelectionSource,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity();
        let text_view = view.clone();
        let image_view = view;
        Button::new(id)
            .small()
            .outline()
            .label(label)
            .dropdown_menu(move |menu, _, _| {
                menu.item(
                    PopupMenuItem::new(crate::fl!("pdf-export-as-text")).on_click({
                        let view = text_view.clone();
                        move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.export_selected_pages_as_text(source, cx);
                            });
                        }
                    }),
                )
                .item(
                    PopupMenuItem::new(crate::fl!("pdf-export-as-image")).on_click({
                        let view = image_view.clone();
                        move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.export_selected_pages_as_image(source, cx);
                            });
                        }
                    }),
                )
            })
    }

    pub(super) fn render_note_popup(
        body: SharedString,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut note = div()
            .occlude()
            .flex()
            .flex_col()
            .gap_1()
            .max_w(px(360.0))
            .p_3()
            .popover_style(cx)
            .text_size(ui_rems(13.0))
            .text_color(theme.foreground);

        for line in body.as_ref().lines() {
            note = note.child(div().child(line.to_string()));
        }

        note
    }

    pub(super) fn render_centered_overlay(
        content: impl IntoElement + 'static,
        close: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.overlay)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| close(this, window, cx)),
            )
            .on_scroll_wheel(|_, _, cx| {
                cx.stop_propagation();
            })
            .child(content)
    }

    pub(super) fn render_sidebar_shell(
        title: String,
        header_end: impl IntoElement + 'static,
        content: impl IntoElement + 'static,
        theme: ThemeColor,
        document_view: gpui::WeakEntity<Self>,
    ) -> impl IntoElement {
        div()
            .relative()
            .flex()
            .flex_1()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .border_r(px(1.0))
            .border_color(theme.border)
            .bg(theme.popover)
            .text_size(ui_rems(12.0))
            .text_color(theme.foreground)
            .capture_any_mouse_down(move |_: &MouseDownEvent, _, cx| {
                let _ = document_view.update(cx, |this, cx| {
                    this.close_context_menu(cx);
                });
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
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
                    .gap_2()
                    .h(px(40.0))
                    .px_3()
                    .border_b(px(1.0))
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_rems(13.0))
                            .child(title),
                    )
                    .child(div().flex_none().child(header_end)),
            )
            .child(content)
    }

    pub(super) fn render_sidebar_count(count: usize, theme: ThemeColor) -> impl IntoElement {
        div()
            .text_color(theme.muted_foreground)
            .child(crate::fl!("item-count", count = count as i64))
    }

    pub(super) fn render_sidebar_empty_message(
        message: String,
        theme: ThemeColor,
    ) -> impl IntoElement {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .px_3()
            .text_color(theme.muted_foreground)
            .child(message)
    }

    fn context_menu_highlight_color_row(
        action: HighlightColorAction,
        current: Option<HighlightColor>,
        document_theme: Theme,
        theme: ThemeColor,
        radius: Pixels,
        view: Entity<Self>,
    ) -> impl IntoElement {
        let mut row = div().flex().flex_row().gap_2().px_3().pb_2();

        for color in HighlightColor::ALL {
            row = row.child(Self::context_menu_highlight_swatch(
                color,
                current,
                action,
                document_theme,
                theme,
                radius,
                view.clone(),
            ));
        }

        row
    }

    fn context_menu_highlight_swatch(
        color: HighlightColor,
        current: Option<HighlightColor>,
        action: HighlightColorAction,
        document_theme: Theme,
        theme: ThemeColor,
        radius: Pixels,
        view: Entity<Self>,
    ) -> impl IntoElement {
        div()
            .id(format!("highlight-swatch-{action:?}-{color:?}"))
            .w(px(22.0))
            .h(px(22.0))
            .rounded(radius)
            .border_1()
            .border_color(if current == Some(color) {
                theme.primary
            } else {
                theme.border
            })
            .bg(rgba(document_theme.highlight_color(color)))
            .cursor_pointer()
            .hover(move |style| style.border_color(theme.primary))
            .on_click(move |_, _, cx| {
                view.update(cx, |this, cx| {
                    match action {
                        HighlightColorAction::Create => {
                            this.create_highlight_from_selection(color, cx);
                        }
                        HighlightColorAction::Change => {
                            this.set_context_highlight_color(color, cx);
                        }
                    }
                    this.close_context_menu(cx);
                });
            })
    }

    fn build_context_menu_popup(
        mut menu: PopupMenu,
        state: ContextMenuState,
        is_fullscreen: bool,
        document_theme: Theme,
        view: Entity<Self>,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let theme = cx.theme().colors;
        let radius = cx.theme().radius;
        let fullscreen_label = if is_fullscreen {
            crate::fl!("pdf-context-exit-full-screen")
        } else {
            crate::fl!("pdf-context-enter-full-screen")
        };

        menu = menu.min_w(px(190.0));

        if state.has_note {
            menu = menu
                .item(Self::context_menu_item(
                    crate::fl!("pdf-context-edit-note"),
                    view.clone(),
                    |this, window, cx| this.begin_edit_context_note(window, cx),
                ))
                .item(Self::context_menu_item(
                    crate::fl!("pdf-context-remove-note"),
                    view.clone(),
                    |this, _, cx| this.remove_context_note(cx),
                ))
                .separator();
        } else if state.has_selection {
            menu = menu
                .item(Self::context_menu_item(
                    crate::fl!("pdf-context-add-note"),
                    view.clone(),
                    |this, window, cx| this.begin_add_note(window, cx),
                ))
                .separator();
        }

        if let Some(color) = state.highlight_color {
            let color_view = view.clone();
            menu = menu
                .label(crate::fl!("pdf-context-change-highlight-color"))
                .item(PopupMenuItem::element(move |_, _| {
                    Self::context_menu_highlight_color_row(
                        HighlightColorAction::Change,
                        Some(color),
                        document_theme,
                        theme,
                        radius,
                        color_view.clone(),
                    )
                }))
                .item(Self::context_menu_item(
                    crate::fl!("pdf-context-remove-highlight"),
                    view.clone(),
                    |this, _, cx| this.remove_context_highlight(cx),
                ))
                .separator();
        } else if state.has_selection {
            let color_view = view.clone();
            menu = menu
                .label(crate::fl!("pdf-context-highlight"))
                .item(PopupMenuItem::element(move |_, _| {
                    Self::context_menu_highlight_color_row(
                        HighlightColorAction::Create,
                        None,
                        document_theme,
                        theme,
                        radius,
                        color_view.clone(),
                    )
                }))
                .separator();
        }

        if state.has_image {
            menu = menu
                .item(Self::context_menu_item(
                    crate::fl!("pdf-context-copy-image"),
                    view.clone(),
                    |this, _, cx| this.copy_image(cx),
                ))
                .item(Self::context_menu_item(
                    crate::fl!("pdf-context-save-image"),
                    view.clone(),
                    |this, _, cx| this.save_image(cx),
                ))
                .separator();
        }

        if state.has_region {
            menu = menu
                .item(Self::context_menu_item(
                    crate::fl!("pdf-context-copy-region"),
                    view.clone(),
                    |this, _, cx| this.copy_region(cx),
                ))
                .item(Self::context_menu_item(
                    crate::fl!("pdf-context-save-region"),
                    view.clone(),
                    |this, _, cx| this.save_region(cx),
                ))
                .separator();
        }

        if state.has_page {
            menu = menu
                .item(Self::context_menu_item(
                    crate::fl!("pdf-context-save-page-as-image"),
                    view.clone(),
                    |this, _, cx| this.save_page_as_image(cx),
                ))
                .separator();
        }

        if state.highlight_color.is_none() && state.has_selection {
            menu = menu.item(Self::context_menu_item(
                crate::fl!("pdf-context-translate-selection"),
                view.clone(),
                |this, _, cx| {
                    this.translate_selection(this.context_menu_position, false, cx);
                },
            ));
            let plain_view = view.clone();
            let markdown_view = view.clone();
            menu = menu
                .submenu(crate::fl!("copy"), window, cx, move |submenu, _, _| {
                    submenu
                        .item(Self::context_menu_item(
                            crate::fl!("pdf-context-copy-plain-text"),
                            plain_view.clone(),
                            |this, _, cx| this.copy_selection_as(CopyFormat::PlainText, cx),
                        ))
                        .item(Self::context_menu_item(
                            crate::fl!("pdf-context-copy-markdown"),
                            markdown_view.clone(),
                            |this, _, cx| this.copy_selection_as(CopyFormat::Markdown, cx),
                        ))
                })
                .separator();
        }

        if !state.grid_overview {
            menu = menu
                .item(Self::context_menu_item(
                    crate::fl!("select-all"),
                    view.clone(),
                    |this, _, cx| {
                        this.select_all();
                        cx.notify();
                    },
                ))
                .separator();
        }

        menu = menu
            .item(Self::context_menu_item(
                crate::fl!("zoom-in"),
                view.clone(),
                |this, _, cx| this.zoom_in(cx),
            ))
            .item(Self::context_menu_item(
                crate::fl!("zoom-out"),
                view.clone(),
                |this, _, cx| this.zoom_out(cx),
            ))
            .item(Self::context_menu_item(
                crate::fl!("reset-zoom"),
                view.clone(),
                |this, _, cx| this.reset_zoom(cx),
            ))
            .separator()
            .item(Self::context_menu_item(
                fullscreen_label,
                view.clone(),
                |_, window, _| window.toggle_fullscreen(),
            ))
            .menu(
                crate::fl!("pdf-context-toggle-zen-mode"),
                Box::new(ToggleZenMode),
            )
            .item(Self::context_menu_item(
                crate::fl!("settings-title"),
                view,
                |_, _, cx| cx.emit(DocumentViewEvent::OpenSettings),
            ));

        menu
    }
}
