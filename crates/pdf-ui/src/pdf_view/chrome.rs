use super::*;

impl PdfView {
    pub(super) fn render_loading_state(
        &self,
        theme: ThemeColor,
        opacity: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let content = if let Some(error) = &self.loading_error {
            Alert::error("pdf-load-error", error.clone())
                .title(crate::fl!("pdf-failed-to-load"))
                .small()
                .max_w(px(520.0))
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
                                        .bg(theme.primary)
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
                        .text_color(theme.muted_foreground)
                        .child(crate::fl!("pdf-loading", name = self.file_name.as_ref())),
                )
                .child(
                    div()
                        .text_size(ui_rems(12.0))
                        .text_color(theme.muted_foreground)
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
            .bg(theme.tab_active)
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
        theme: ThemeColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let resizable_state = self.sync_sidebar_resizable_state(cx);
        let max_sidebar_width =
            (window.bounds().size.width.as_f32() * SIDEBAR_MAX_WIDTH_RATIO).max(SIDEBAR_MIN_WIDTH);
        let sidebar_size_range = px(SIDEBAR_MIN_WIDTH)..px(max_sidebar_width);
        let mut panels = Vec::with_capacity(3);

        if self.thumbnail_sidebar_open {
            panels.push(
                resizable_panel()
                    .size(px(self.thumbnail_sidebar_width))
                    .size_range(sidebar_size_range.clone())
                    .flex_none()
                    .child(self.render_thumbnail_sidebar(theme, window, cx)),
            );
        } else if self.toc_sidebar_open {
            panels.push(
                resizable_panel()
                    .size(px(self.toc_sidebar_width))
                    .size_range(sidebar_size_range.clone())
                    .flex_none()
                    .child(self.render_toc_sidebar(theme, window, cx)),
            );
        } else if self.notes_sidebar_open {
            panels.push(
                resizable_panel()
                    .size(px(self.notes_sidebar_width))
                    .size_range(sidebar_size_range.clone())
                    .flex_none()
                    .child(self.render_notes_sidebar(theme, window, cx)),
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

        if let Some(panel) = self.ai_panel.clone() {
            panels.push(
                resizable_panel()
                    .size(px(self.ai_panel_width))
                    .size_range(sidebar_size_range)
                    .flex_none()
                    .child(div().size_full().min_w_0().child(panel)),
            );
        }

        let resizable = ResizablePanelGroup::new("pdf-workspace-panels")
            .with_state(&resizable_state)
            .children(panels);

        div()
            .size_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(theme.tab_active)
            .child(div().flex_1().min_h_0().child(resizable))
            .child(self.render_workspace_footbar(theme, cx))
            .into_any_element()
    }

    pub fn render_workspace_footbar(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
            .border_color(theme.border)
            .bg(theme.status_bar)
            .text_size(ui_rems(12.0))
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(self.render_toc_button(cx))
                    .child(self.render_notes_button(cx))
                    .when(self.capabilities.page_overview, |this| {
                        this.child(self.render_thumbnail_sidebar_button(cx))
                            .child(self.render_grid_overview_button(cx))
                    })
                    .when(
                        self.capabilities.page_export && !self.grid_overview_open,
                        |this| this.child(self.render_region_button(cx)),
                    )
                    .when(self.capabilities.reflow, |this| {
                        this.child(self.render_epub_style_button(cx))
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
                    .child(self.render_ai_button(cx)),
            )
            .into_any_element()
    }
}
