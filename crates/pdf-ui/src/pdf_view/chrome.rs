use super::*;

impl PdfView {
    pub(super) fn render_loading_state(
        &self,
        theme: Theme,
        opacity: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let content = if let Some(error) = &self.loading_error {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .text_align(gpui::TextAlign::Center)
                .child(
                    div()
                        .text_size(ui_rems(15.0))
                        .text_color(rgb(theme.error))
                        .child(crate::fl!("pdf-failed-to-load")),
                )
                .child(
                    div()
                        .max_w(px(520.0))
                        .text_size(ui_rems(12.0))
                        .text_color(rgb(theme.text_muted))
                        .child(error.clone()),
                )
                .into_any_element()
        } else {
            const GRID_SQUARE_SIZE: f32 = 18.0;
            const GRID_GAP: f32 = 4.0;
            const GRID_PHASES: [f32; 9] = [0.80, 0.05, 0.50, 0.30, 0.95, 0.15, 0.65, 0.40, 0.70];
            const GRID_DURATION_MS: u64 = 1200;

            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .text_align(gpui::TextAlign::Center)
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(GRID_GAP))
                        .w(px(GRID_SQUARE_SIZE * 3.0 + GRID_GAP * 3.0))
                        .items_center()
                        .justify_center()
                        .children((0..9).map(|index| {
                            let phase = GRID_PHASES[index];

                            div()
                                .w(px(GRID_SQUARE_SIZE))
                                .h(px(GRID_SQUARE_SIZE))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .w(px(GRID_SQUARE_SIZE))
                                        .h(px(GRID_SQUARE_SIZE))
                                        .bg(rgb(theme.accent))
                                        .with_animation(
                                            ("pdf-grid-loader-square", index),
                                            Animation::new(Duration::from_millis(GRID_DURATION_MS))
                                                .repeat(),
                                            move |element, delta| {
                                                let t = (delta + phase) % 1.0;
                                                let wave =
                                                    (1.0 - (std::f32::consts::TAU * t).cos()) / 2.0;

                                                element.opacity(0.2 + 0.8 * wave)
                                            },
                                        ),
                                )
                        })),
                )
                .child(
                    div()
                        .text_size(ui_rems(14.0))
                        .text_color(rgb(theme.text_muted))
                        .child(crate::fl!("pdf-loading", name = self.file_name.as_ref())),
                )
                .child(
                    div()
                        .text_size(ui_rems(12.0))
                        .text_color(rgb(theme.text_muted))
                        .child(format!("{} pages", self.page_count)),
                )
                .into_any_element()
        };

        div()
            .size_full()
            .key_context("PdfView")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Reload, _, cx| {
                this.request_reload(cx);
            }))
            .flex()
            .flex_col()
            .bg(rgb(theme.tab_active_background))
            .when(opacity < 1.0, |this| this.opacity(opacity))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_6()
                    .child(content),
            )
            .into_any_element()
    }

    pub fn has_global_chrome(&self) -> bool {
        !self.is_presentation_mode() && !self.loading && self.loading_error.is_none()
    }

    pub fn render_workspace_body_with_chrome(
        &mut self,
        center: AnyElement,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .size_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(theme.tab_active_background))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .on_drag_move(
                        cx.listener(|this, e: &DragMoveEvent<DraggedSidebar>, _, cx| {
                            let dragged_sidebar = *e.drag(cx);
                            let max_width = (e.bounds.size.width.as_f32()
                                * SIDEBAR_MAX_WIDTH_RATIO)
                                .max(SIDEBAR_MIN_WIDTH);
                            let new_width = match dragged_sidebar {
                                DraggedSidebar::Thumbnails => {
                                    (e.event.position.x - e.bounds.left()).as_f32()
                                }
                                DraggedSidebar::Toc => {
                                    let thumbnail_offset = if this.thumbnail_sidebar_open {
                                        this.thumbnail_sidebar_width
                                    } else {
                                        0.0
                                    };
                                    (e.event.position.x - e.bounds.left()).as_f32()
                                        - thumbnail_offset
                                }
                                DraggedSidebar::Notes => {
                                    let thumbnail_offset = if this.thumbnail_sidebar_open {
                                        this.thumbnail_sidebar_width
                                    } else {
                                        0.0
                                    };
                                    let toc_offset = if this.toc_sidebar_open {
                                        this.toc_sidebar_width
                                    } else {
                                        0.0
                                    };
                                    (e.event.position.x - e.bounds.left()).as_f32()
                                        - thumbnail_offset
                                        - toc_offset
                                }
                                DraggedSidebar::AiPanel => {
                                    (e.bounds.right() - e.event.position.x).as_f32()
                                }
                            }
                            .clamp(SIDEBAR_MIN_WIDTH, max_width);

                            match dragged_sidebar {
                                DraggedSidebar::Thumbnails => {
                                    this.thumbnail_sidebar_width = new_width;
                                }
                                DraggedSidebar::Toc => {
                                    this.toc_sidebar_width = new_width;
                                }
                                DraggedSidebar::Notes => {
                                    this.notes_sidebar_width = new_width;
                                }
                                DraggedSidebar::AiPanel => {
                                    this.ai_panel_width = new_width;
                                    if let Some(panel) = &this.ai_panel {
                                        panel.update(cx, |panel, cx| {
                                            panel.set_width(new_width);
                                            cx.notify();
                                        });
                                    }
                                }
                            }
                            this.schedule_sidebar_width_save(cx);
                            cx.notify();
                        }),
                    )
                    .when(self.thumbnail_sidebar_open, |this| {
                        this.child(self.render_thumbnail_sidebar(theme, window, cx))
                    })
                    .when(self.toc_sidebar_open, |this| {
                        this.child(self.render_toc_sidebar(theme, window, cx))
                    })
                    .when(self.notes_sidebar_open, |this| {
                        this.child(self.render_notes_sidebar(theme, window, cx))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_hidden()
                            .child(center),
                    )
                    .when_some(self.ai_panel.clone(), |this, panel| {
                        this.child(
                            div()
                                .relative()
                                .flex()
                                .flex_none()
                                .min_h_0()
                                .h_full()
                                .w(px(self.ai_panel_width))
                                .child(panel)
                                .child(Self::render_sidebar_resize_handle(
                                    "ai-panel-resize-handle",
                                    DraggedSidebar::AiPanel,
                                    true,
                                    cx,
                                )),
                        )
                    }),
            )
            .child(self.render_workspace_footbar(theme, cx))
            .into_any_element()
    }

    pub fn render_workspace_footbar(&self, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let background_activity =
            ActivityCenter::try_global(cx).and_then(|center| center.read(cx).render_activity());
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .w_full()
            .flex_none()
            .h(ui_rems(FOOTBAR_HEIGHT))
            .px_3()
            .border_t(px(1.0))
            .border_color(rgb(theme.border))
            .bg(rgb(theme.tab_bar_background))
            .text_size(ui_rems(12.0))
            .text_color(rgb(theme.footer_text))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(self.render_toc_button(theme, cx))
                    .child(self.render_notes_button(theme, cx))
                    .when(self.capabilities.page_overview, |this| {
                        this.child(self.render_thumbnail_sidebar_button(theme, cx))
                            .child(self.render_grid_overview_button(theme, cx))
                    })
                    .when(
                        self.capabilities.page_export && !self.grid_overview_open,
                        |this| this.child(self.render_region_button(theme, cx)),
                    )
                    .when(self.capabilities.reflow, |this| {
                        this.child(self.render_epub_style_button(theme, cx))
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when_some(background_activity, |this, activity| this.child(activity))
                    .child(Self::page_counter_label(
                        self.current_page(),
                        self.pages.len(),
                    ))
                    .child(self.render_ai_button(theme, cx)),
            )
            .into_any_element()
    }
}
