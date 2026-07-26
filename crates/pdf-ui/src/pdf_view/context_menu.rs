use super::*;

impl PdfView {
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
        self.grid_overview_actions_position = None;
        self.thumbnail_sidebar_actions_position = None;
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
        self.context_menu_copy_submenu_open = false;
        self.context_menu_position = Some(position);
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn close_context_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let was_open = self.context_menu_position.take().is_some();
        self.context_menu_page = None;
        self.context_menu_image = None;
        self.context_menu_highlight = None;
        self.context_menu_region = false;
        self.context_menu_copy_submenu_open = false;
        if was_open {
            self.context_menu_note = None;
            cx.notify();
        }
        was_open
    }

    fn context_menu_entry(
        label: String,
        theme: Theme,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(label.clone())
            .flex()
            .items_center()
            .h(px(30.0))
            .px_3()
            .text_size(ui_rems(13.0))
            .text_color(rgb(theme.text))
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    handler(this, window, cx);
                    this.close_context_menu(cx);
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn context_menu_copy_entry(
        theme: Theme,
        copy_submenu_open: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let submenu = div()
            .absolute()
            .left(px(188.0))
            .top_0()
            .w(px(190.0))
            .py_1()
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .child(Self::context_menu_entry(
                crate::fl!("pdf-context-copy-plain-text"),
                theme,
                |this, _, cx| this.copy_selection_as(CopyFormat::PlainText, cx),
                cx,
            ))
            .child(Self::context_menu_entry(
                crate::fl!("pdf-context-copy-markdown"),
                theme,
                |this, _, cx| this.copy_selection_as(CopyFormat::Markdown, cx),
                cx,
            ));

        div()
            .relative()
            .when(copy_submenu_open, |this| this.child(submenu))
            .child(
                div()
                    .id("pdf-context-copy")
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(30.0))
                    .px_3()
                    .text_size(ui_rems(13.0))
                    .text_color(rgb(theme.text))
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(theme.element_hover)))
                    .on_hover(cx.listener(|this, hovered, _, cx| {
                        if *hovered && !this.context_menu_copy_submenu_open {
                            this.context_menu_copy_submenu_open = true;
                            cx.notify();
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.copy_selection(cx);
                            this.close_context_menu(cx);
                            window.prevent_default();
                            cx.stop_propagation();
                        }),
                    )
                    .child(crate::fl!("copy"))
                    .child("›"),
            )
    }

    fn context_menu_label(label: String, theme: Theme) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .h(px(24.0))
            .px_3()
            .text_size(ui_rems(12.0))
            .text_color(rgb(theme.text_muted))
            .child(label)
    }

    pub(super) fn grid_overview_toolbar_button(
        id: &'static str,
        label: String,
        theme: Theme,
        handler: impl Fn(&mut Self, &MouseDownEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .h(px(24.0))
            .px_2()
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgba(0x00000000))
            .text_size(ui_rems(12.0))
            .text_color(rgb(theme.text))
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    handler(this, event, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn grid_overview_action_entry(
        label: String,
        theme: Theme,
        handler: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(label.clone())
            .flex()
            .items_center()
            .h(px(30.0))
            .px_3()
            .text_size(ui_rems(13.0))
            .text_color(rgb(theme.text))
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    handler(this, cx);
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn render_page_selection_actions_menu(
        source: PageSelectionSource,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let text_source = source;
        let image_source = source;

        div()
            .occlude()
            .flex()
            .flex_col()
            .min_w(px(160.0))
            .py_1()
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .child(Self::grid_overview_action_entry(
                crate::fl!("pdf-export-as-text"),
                theme,
                move |this, cx| this.export_selected_pages_as_text(text_source, cx),
                cx,
            ))
            .child(Self::grid_overview_action_entry(
                crate::fl!("pdf-export-as-image"),
                theme,
                move |this, cx| this.export_selected_pages_as_image(image_source, cx),
                cx,
            ))
    }

    pub(super) fn render_grid_overview_actions_menu(
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Self::render_page_selection_actions_menu(PageSelectionSource::GridOverview, theme, cx)
    }

    pub(super) fn render_thumbnail_sidebar_actions_menu(
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Self::render_page_selection_actions_menu(PageSelectionSource::ThumbnailSidebar, theme, cx)
    }

    pub(super) fn render_note_popup(body: SharedString, theme: Theme) -> impl IntoElement {
        let mut note = div()
            .occlude()
            .flex()
            .flex_col()
            .gap_1()
            .max_w(px(360.0))
            .p_3()
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .text_size(ui_rems(13.0))
            .text_color(rgb(theme.text));

        for line in body.as_ref().lines() {
            note = note.child(div().child(line.to_string()));
        }

        note
    }

    pub(super) fn render_centered_overlay(
        content: impl IntoElement + 'static,
        close: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        theme: Theme,
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
            .bg(rgba(theme.overlay))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| close(this, window, cx)),
            )
            .on_scroll_wheel(|_, _, cx| {
                cx.stop_propagation();
            })
            .child(content)
    }

    pub(super) fn render_sidebar_resize_handle(
        id: &'static str,
        sidebar: DraggedSidebar,
        on_left: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + 'static {
        let offset = px(-SIDEBAR_RESIZE_HANDLE_SIZE / 2.0);
        let handle = div()
            .id(id)
            .on_drag(sidebar, |sidebar, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| *sidebar)
            })
            .capture_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.close_context_menu(cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                }),
            )
            .absolute()
            .top(px(0.0))
            .h_full()
            .w(px(SIDEBAR_RESIZE_HANDLE_SIZE))
            .cursor_col_resize()
            .occlude();

        if on_left {
            deferred(handle.left(offset))
        } else {
            deferred(handle.right(offset))
        }
    }

    pub(super) fn render_sidebar_shell(
        title: String,
        width: f32,
        header_end: impl IntoElement + 'static,
        content: impl IntoElement + 'static,
        resize_handle: impl IntoElement + 'static,
        theme: Theme,
        pdf_view: gpui::WeakEntity<Self>,
    ) -> impl IntoElement {
        div()
            .relative()
            .flex()
            .flex_none()
            .flex_col()
            .min_h_0()
            .w(px(width))
            .border_r(px(1.0))
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .text_size(ui_rems(12.0))
            .text_color(rgb(theme.text))
            .capture_any_mouse_down(move |_: &MouseDownEvent, _, cx| {
                let _ = pdf_view.update(cx, |this, cx| {
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
                    .border_color(rgb(theme.border))
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
            .child(resize_handle)
    }

    pub(super) fn render_sidebar_count(count: usize, theme: Theme) -> impl IntoElement {
        div()
            .text_color(rgb(theme.text_muted))
            .child(crate::fl!("item-count", count = count as i64))
    }

    pub(super) fn render_sidebar_empty_message(message: String, theme: Theme) -> impl IntoElement {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .px_3()
            .text_color(rgb(theme.text_muted))
            .child(message)
    }

    fn context_menu_highlight_color_row(
        action: HighlightColorAction,
        current: Option<HighlightColor>,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut row = div().flex().flex_row().gap_2().px_3().pb_2();

        for color in HighlightColor::ALL {
            row = row.child(Self::context_menu_highlight_swatch(
                color, current, action, theme, cx,
            ));
        }

        row
    }

    fn context_menu_highlight_swatch(
        color: HighlightColor,
        current: Option<HighlightColor>,
        action: HighlightColorAction,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(format!("highlight-swatch-{action:?}-{color:?}"))
            .w(px(22.0))
            .h(px(22.0))
            .border_1()
            .border_color(if current == Some(color) {
                rgb(theme.accent)
            } else {
                rgb(theme.border)
            })
            .bg(rgba(theme.highlight_color(color)))
            .cursor_pointer()
            .hover(move |style| style.border_color(rgb(theme.accent)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    match action {
                        HighlightColorAction::Create => {
                            this.create_highlight_from_selection(color, cx);
                        }
                        HighlightColorAction::Change => {
                            this.set_context_highlight_color(color, cx);
                        }
                    }
                    this.close_context_menu(cx);
                    cx.stop_propagation();
                }),
            )
    }

    fn context_menu_separator(theme: Theme) -> impl IntoElement {
        div().h(px(1.0)).my_1().mx_1().bg(rgb(theme.border))
    }

    pub(super) fn render_context_menu(
        state: ContextMenuState,
        is_fullscreen: bool,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let fullscreen_label = if is_fullscreen {
            crate::fl!("pdf-context-exit-full-screen")
        } else {
            crate::fl!("pdf-context-enter-full-screen")
        };

        div()
            .occlude()
            .flex()
            .flex_col()
            .w(px(190.0))
            .py_1()
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_mouse_down(MouseButton::Middle, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_mouse_down(MouseButton::Right, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .when(state.has_note, |menu| {
                menu.child(Self::context_menu_entry(
                    crate::fl!("pdf-context-edit-note"),
                    theme,
                    |this, window, cx| this.begin_edit_context_note(window, cx),
                    cx,
                ))
                .child(Self::context_menu_entry(
                    crate::fl!("pdf-context-remove-note"),
                    theme,
                    |this, _, cx| this.remove_context_note(cx),
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .when(!state.has_note && state.has_selection, |menu| {
                menu.child(Self::context_menu_entry(
                    crate::fl!("pdf-context-add-note"),
                    theme,
                    |this, window, cx| this.begin_add_note(window, cx),
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .when_some(state.highlight_color, |menu, color| {
                menu.child(Self::context_menu_label(
                    crate::fl!("pdf-context-change-highlight-color"),
                    theme,
                ))
                .child(Self::context_menu_highlight_color_row(
                    HighlightColorAction::Change,
                    Some(color),
                    theme,
                    cx,
                ))
                .child(Self::context_menu_entry(
                    crate::fl!("pdf-context-remove-highlight"),
                    theme,
                    |this, _, cx| this.remove_context_highlight(cx),
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .when(
                state.highlight_color.is_none() && state.has_selection,
                |menu| {
                    menu.child(Self::context_menu_label(
                        crate::fl!("pdf-context-highlight"),
                        theme,
                    ))
                    .child(Self::context_menu_highlight_color_row(
                        HighlightColorAction::Create,
                        None,
                        theme,
                        cx,
                    ))
                    .child(Self::context_menu_separator(theme))
                },
            )
            .when(state.has_image, |menu| {
                menu.child(Self::context_menu_entry(
                    crate::fl!("pdf-context-copy-image"),
                    theme,
                    |this, _, cx| this.copy_image(cx),
                    cx,
                ))
                .child(Self::context_menu_entry(
                    crate::fl!("pdf-context-save-image"),
                    theme,
                    |this, _, cx| this.save_image(cx),
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .when(state.has_region, |menu| {
                menu.child(Self::context_menu_entry(
                    crate::fl!("pdf-context-copy-region"),
                    theme,
                    |this, _, cx| this.copy_region(cx),
                    cx,
                ))
                .child(Self::context_menu_entry(
                    crate::fl!("pdf-context-save-region"),
                    theme,
                    |this, _, cx| this.save_region(cx),
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .when(state.has_page, |menu| {
                menu.child(Self::context_menu_entry(
                    crate::fl!("pdf-context-save-page-as-image"),
                    theme,
                    |this, _, cx| this.save_page_as_image(cx),
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .when(
                state.highlight_color.is_none() && state.has_selection,
                |menu| {
                    menu.child(Self::context_menu_entry(
                        crate::fl!("pdf-context-translate-selection"),
                        theme,
                        |this, _, cx| {
                            let position = this.context_menu_position;
                            this.close_context_menu(cx);
                            this.translate_selection(position, false, cx);
                        },
                        cx,
                    ))
                    .child(Self::context_menu_copy_entry(
                        theme,
                        state.copy_submenu_open,
                        cx,
                    ))
                    .child(Self::context_menu_separator(theme))
                },
            )
            .when(!state.grid_overview, |menu| {
                menu.child(Self::context_menu_entry(
                    crate::fl!("select-all"),
                    theme,
                    |this, _, cx| {
                        this.select_all();
                        cx.notify();
                    },
                    cx,
                ))
                .child(Self::context_menu_separator(theme))
            })
            .child(Self::context_menu_entry(
                crate::fl!("zoom-in"),
                theme,
                |this, _, cx| this.zoom_in(cx),
                cx,
            ))
            .child(Self::context_menu_entry(
                crate::fl!("zoom-out"),
                theme,
                |this, _, cx| this.zoom_out(cx),
                cx,
            ))
            .child(Self::context_menu_entry(
                crate::fl!("reset-zoom"),
                theme,
                |this, _, cx| this.reset_zoom(cx),
                cx,
            ))
            .child(Self::context_menu_separator(theme))
            .child(Self::context_menu_entry(
                fullscreen_label,
                theme,
                |_, window, _| window.toggle_fullscreen(),
                cx,
            ))
            .child(Self::context_menu_entry(
                crate::fl!("settings-title"),
                theme,
                |_, _, cx| cx.emit(PdfViewEvent::OpenSettings),
                cx,
            ))
    }
}
