use super::*;

impl DocumentView {
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

    fn render_footbar_icon_button(
        id: &'static str,
        icon: IconName,
        open: bool,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(id)
            .xsmall()
            .compact()
            .ghost()
            .selected(open)
            .icon(icon)
            .h(ui_rems(FOOTBAR_BUTTON_HEIGHT))
            .px_2()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.close_epub_style_panel(cx);
                handler(this, window, cx);
                cx.stop_propagation();
            }))
    }

    pub(super) fn render_grid_overview_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Self::render_footbar_icon_button(
            "footbar-grid-overview",
            IconName::LayoutGrid,
            self.grid_overview_open,
            |this, _, cx| this.toggle_grid_overview(cx),
            cx,
        )
    }

    pub(super) fn render_two_column_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Self::render_footbar_icon_button(
            "footbar-two-column",
            IconName::BookOpen,
            self.is_two_column_layout(),
            |this, _, cx| this.toggle_two_column_mode(cx),
            cx,
        )
        .tooltip(crate::fl!("pdf-two-column-tooltip"))
    }

    pub(super) fn render_ai_button(&self, open: bool, cx: &mut Context<Self>) -> impl IntoElement {
        Self::render_footbar_icon_button(
            "footbar-ai",
            IconName::Bot,
            open,
            |_, window, cx| window.dispatch_action(Box::new(ToggleAiPanel), cx),
            cx,
        )
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
        Self::render_footbar_icon_button(
            "footbar-region-selection",
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
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.save_region(cx);
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
            .h(ui_rems(FOOTBAR_BUTTON_HEIGHT))
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
        Button::new("footbar-epub-style")
            .xsmall()
            .compact()
            .ghost()
            .selected(open)
            .icon(IconName::Settings)
            .h(ui_rems(FOOTBAR_BUTTON_HEIGHT))
            .px_2()
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
                    value: format!("{:.0}", preferences.font_size),
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
                    value: format!("{:.1}", preferences.line_height),
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
                    value: format!("{:.0} px", preferences.horizontal_margin),
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
                    value: format!("{:.0} px", preferences.max_content_width),
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
                    .child(
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
                            )),
                    ),
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
