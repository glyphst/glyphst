use super::*;

impl DocumentView {
    pub fn has_global_chrome(&self) -> bool {
        !self.is_presentation_mode()
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

        if self.ai_panel_visible
            && let Some(panel) = self.ai_panel.clone()
        {
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
