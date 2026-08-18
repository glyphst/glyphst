use super::*;

impl DocumentView {
    pub(super) fn render_zoom_popup(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let percentage = format!("{}%", format_user_float((self.zoom * 100.0) as f64));

        div()
            .id("document-zoom-popup")
            .debug_selector(|| "document-zoom-popup".to_string())
            .occlude()
            .flex()
            .items_center()
            .gap_1()
            .p_1()
            .popover_style(cx)
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_scroll_wheel(|_, _, cx| {
                cx.stop_propagation();
            })
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.set_zoom_popup_hovered(*hovered, cx);
            }))
            .child(
                toolbar_icon_button("zoom-popup-out", IconName::Minus)
                    .debug_selector(|| "zoom-popup-out".to_string())
                    .disabled(self.zoom <= MIN_ZOOM)
                    .tooltip(crate::fl!("zoom-out"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.zoom_out(cx);
                        cx.stop_propagation();
                    })),
            )
            .child(
                Button::new("zoom-popup-reset")
                    .debug_selector(|| "zoom-popup-reset".to_string())
                    .small()
                    .compact()
                    .ghost()
                    .label(percentage)
                    .min_w(px(54.0))
                    .h(ui_rems(CONTROL_SIZE))
                    .px_2()
                    .tooltip(crate::fl!("reset-zoom"))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.reset_zoom(cx);
                        cx.stop_propagation();
                    })),
            )
            .child(
                toolbar_icon_button("zoom-popup-in", IconName::Plus)
                    .debug_selector(|| "zoom-popup-in".to_string())
                    .disabled(self.zoom >= MAX_ZOOM)
                    .tooltip(crate::fl!("zoom-in"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.zoom_in(cx);
                        cx.stop_propagation();
                    })),
            )
            .when(!self.capabilities.reflow, |this| {
                this.child(div().w(px(1.0)).h(px(18.0)).mx_1().bg(theme.border))
                    .child(
                        toolbar_icon_button("zoom-popup-fit-width", IconName::MoveHorizontal)
                            .debug_selector(|| "zoom-popup-fit-width".to_string())
                            .tooltip(crate::fl!("zoom-fit-width"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.fit_to_width(cx);
                                cx.stop_propagation();
                            })),
                    )
                    .child(
                        toolbar_icon_button("zoom-popup-fit-height", IconName::MoveVertical)
                            .debug_selector(|| "zoom-popup-fit-height".to_string())
                            .tooltip(crate::fl!("zoom-fit-height"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.fit_to_height(cx);
                                cx.stop_propagation();
                            })),
                    )
            })
    }

    pub(super) fn ensure_toc_sidebar_visible_entries(&mut self) {
        if !self.toc_sidebar_entries_dirty {
            return;
        }

        self.toc_sidebar_visible_entry_ixs.clear();
        let mut hidden_depth = None;
        for (entry_ix, entry) in self.toc.iter().enumerate() {
            if let Some(depth) = hidden_depth {
                if entry.depth > depth {
                    continue;
                }
                hidden_depth = None;
            }

            self.toc_sidebar_visible_entry_ixs.push(entry_ix);
            if self.toc_collapsed.contains(&entry_ix) {
                hidden_depth = Some(entry.depth);
            }
        }
        self.toc_sidebar_entries_dirty = false;
    }

    fn render_document_toolbar_icon_button(
        id: &'static str,
        icon: IconName,
        open: bool,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(id)
            .small()
            .compact()
            .ghost()
            .selected(open)
            .icon(icon)
            .w(ui_rems(CONTROL_SIZE))
            .h(ui_rems(CONTROL_SIZE))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.close_epub_style_panel(cx);
                handler(this, window, cx);
                this.show_document_toolbar(cx);
                cx.stop_propagation();
            }))
    }

    pub(super) fn render_grid_overview_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Self::render_document_toolbar_icon_button(
            "document-toolbar-grid-overview",
            IconName::LayoutGrid,
            self.grid_overview_open,
            |this, _, cx| this.toggle_grid_overview(cx),
            cx,
        )
    }

    pub(super) fn render_two_column_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Self::render_document_toolbar_icon_button(
            "document-toolbar-two-column",
            IconName::BookOpen,
            self.is_two_column_layout(),
            |this, _, cx| this.toggle_two_column_mode(cx),
            cx,
        )
        .tooltip(crate::fl!("pdf-two-column-tooltip"))
    }

    pub(super) fn toggle_region_selection(&mut self, cx: &mut Context<Self>) {
        if !self.capabilities.page_export || self.grid_overview_open {
            return;
        }
        self.region_tool_active = !self.region_tool_active;
        self.region_drag = None;
        if self.region_tool_active {
            self.selected_region = None;
            self.selection = None;
            self.selected_highlight = None;
        }
        self.close_context_menu(cx);
        cx.notify();
    }

    pub(super) fn cancel_region_selection(&mut self, cx: &mut Context<Self>) {
        if !self.region_tool_active && self.region_drag.is_none() {
            return;
        }
        self.region_tool_active = false;
        self.region_drag = None;
        cx.notify();
    }

    fn dismiss_selected_region(&mut self, cx: &mut Context<Self>) {
        if self.selected_region.take().is_some() {
            self.context_menu_region = false;
            cx.notify();
        }
    }

    pub(super) fn render_region_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Self::render_document_toolbar_icon_button(
            "document-toolbar-region-selection",
            IconName::Scan,
            self.region_tool_active,
            |this, _, cx| this.toggle_region_selection(cx),
            cx,
        )
        .tooltip(crate::fl!("pdf-region-select-tooltip"))
    }

    pub(super) fn render_region_popover(cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .occlude()
            .flex()
            .items_center()
            .gap_1()
            .p_1()
            .popover_style(cx)
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .child(
                toolbar_icon_button("region-popover-copy", IconName::Copy)
                    .tooltip(crate::fl!("pdf-context-copy-region"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.copy_region(cx);
                        cx.stop_propagation();
                    })),
            )
            .child(
                toolbar_icon_button("region-popover-save", IconName::Download)
                    .tooltip(crate::fl!("pdf-context-save-region"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.save_region(window, cx);
                        cx.stop_propagation();
                    })),
            )
            .child(
                toolbar_icon_button("region-popover-dismiss", IconName::X)
                    .tooltip(crate::fl!("pdf-region-dismiss"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.dismiss_selected_region(cx);
                        cx.stop_propagation();
                    })),
            )
    }

    fn render_epub_control(
        id: &'static str,
        label: impl Into<SharedString>,
        active: bool,
        handler: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Button::new(id)
            .xsmall()
            .compact()
            .ghost()
            .selected(active)
            .label(label)
            .h(ui_rems(COMPACT_CONTROL_HEIGHT))
            .min_w(px(24.0))
            .px_2()
            .on_click(cx.listener(move |this, _, _, cx| {
                handler(this, cx);
                cx.stop_propagation();
            }))
    }

    fn render_epub_stepper(
        config: EpubStepperConfig,
        decrease: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        increase: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(div().text_color(theme.foreground).child(config.label))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(Self::render_epub_control(
                        config.decrease_id,
                        "−",
                        false,
                        decrease,
                        cx,
                    ))
                    .child(
                        div()
                            .min_w(px(54.0))
                            .text_center()
                            .text_color(theme.muted_foreground)
                            .child(config.value),
                    )
                    .child(Self::render_epub_control(
                        config.increase_id,
                        "+",
                        false,
                        increase,
                        cx,
                    )),
            )
    }

    fn render_epub_theme_option(
        id: &'static str,
        label: SharedString,
        option: EpubTheme,
        selected: EpubTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Self::render_epub_control(
            id,
            label,
            option == selected,
            move |this, cx| this.set_epub_theme(option, cx),
            cx,
        )
    }

    pub(super) fn render_epub_style_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.epub_style_panel_position.is_some();
        Button::new("document-toolbar-epub-style")
            .small()
            .compact()
            .ghost()
            .selected(open)
            .icon(IconName::Settings)
            .w(ui_rems(CONTROL_SIZE))
            .h(ui_rems(CONTROL_SIZE))
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                this.toggle_epub_style_panel(event.position(), cx);
                cx.stop_propagation();
            }))
    }

    pub(super) fn render_epub_style_panel(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let preferences = self.epub_preferences;
        let follows_app_theme = cx
            .global::<SettingsStore>()
            .settings
            .document_theme
            .follow_app_theme;
        let theme_control = if follows_app_theme {
            div()
                .text_color(theme.muted_foreground)
                .child(crate::fl!("epub-theme-following-app"))
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(Self::render_epub_theme_option(
                    "epub-theme-light",
                    crate::fl!("epub-theme-light").into(),
                    EpubTheme::Light,
                    preferences.theme,
                    cx,
                ))
                .child(Self::render_epub_theme_option(
                    "epub-theme-sepia",
                    crate::fl!("epub-theme-sepia").into(),
                    EpubTheme::Sepia,
                    preferences.theme,
                    cx,
                ))
                .child(Self::render_epub_theme_option(
                    "epub-theme-dark",
                    crate::fl!("epub-theme-dark").into(),
                    EpubTheme::Dark,
                    preferences.theme,
                    cx,
                ))
                .into_any_element()
        };
        div()
            .id("epub-style-panel")
            .occlude()
            .flex()
            .flex_col()
            .gap_3()
            .w(px(340.0))
            .p_3()
            .popover_style(cx)
            .text_size(ui_rems(12.0))
            .text_color(theme.foreground)
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_scroll_wheel(|_, _, cx| {
                cx.stop_propagation();
            })
            .child(
                div()
                    .text_size(ui_rems(13.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(crate::fl!("epub-appearance-title")),
            )
            .child(Self::render_epub_stepper(
                EpubStepperConfig {
                    label: crate::fl!("epub-font-size").into(),
                    value: format_user_float(preferences.font_size as f64),
                    decrease_id: "epub-font-decrease",
                    increase_id: "epub-font-increase",
                },
                |this, cx| this.decrease_epub_font_size(cx),
                |this, cx| this.increase_epub_font_size(cx),
                theme,
                cx,
            ))
            .child(Self::render_epub_stepper(
                EpubStepperConfig {
                    label: crate::fl!("epub-line-height").into(),
                    value: format_user_float(preferences.line_height as f64),
                    decrease_id: "epub-line-decrease",
                    increase_id: "epub-line-increase",
                },
                |this, cx| this.decrease_epub_line_height(cx),
                |this, cx| this.increase_epub_line_height(cx),
                theme,
                cx,
            ))
            .child(Self::render_epub_stepper(
                EpubStepperConfig {
                    label: crate::fl!("epub-horizontal-margin").into(),
                    value: format!(
                        "{} px",
                        format_user_float(preferences.horizontal_margin as f64)
                    ),
                    decrease_id: "epub-margin-decrease",
                    increase_id: "epub-margin-increase",
                },
                |this, cx| this.decrease_epub_horizontal_margin(cx),
                |this, cx| this.increase_epub_horizontal_margin(cx),
                theme,
                cx,
            ))
            .child(Self::render_epub_stepper(
                EpubStepperConfig {
                    label: crate::fl!("epub-max-content-width").into(),
                    value: format!(
                        "{} px",
                        format_user_float(preferences.max_content_width as f64)
                    ),
                    decrease_id: "epub-width-decrease",
                    increase_id: "epub-width-increase",
                },
                |this, cx| this.decrease_epub_max_content_width(cx),
                |this, cx| this.increase_epub_max_content_width(cx),
                theme,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_color(theme.foreground)
                            .child(crate::fl!("epub-theme")),
                    )
                    .child(theme_control),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_color(theme.foreground)
                            .child(crate::fl!("epub-publisher-styles")),
                    )
                    .child(Self::render_epub_control(
                        "epub-publisher-styles",
                        if preferences.use_publisher_styles {
                            crate::fl!("epub-enabled")
                        } else {
                            crate::fl!("epub-disabled")
                        },
                        preferences.use_publisher_styles,
                        |this, cx| this.toggle_epub_publisher_styles(cx),
                        cx,
                    )),
            )
            .child(Self::render_epub_control(
                "epub-reset-defaults",
                crate::fl!("epub-reset-global-defaults"),
                false,
                |this, cx| this.reset_epub_preferences(cx),
                cx,
            ))
    }
}
