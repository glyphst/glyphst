use super::*;

impl DocumentView {
    pub fn render_workspace_footbar(
        &self,
        theme: ThemeColor,
        dock: DocumentDockSnapshot,
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
                    .when(self.capabilities.page_overview, |this| {
                        this.child(self.render_grid_overview_button(cx))
                    })
                    .when(
                        self.capabilities.page_export && !self.grid_overview_open,
                        |this| this.child(self.render_region_button(cx)),
                    )
                    .when(self.capabilities.reflow, |this| {
                        this.child(self.render_epub_style_button(cx))
                    }),
            )
            .child(div().flex_none().text_color(theme.muted_foreground).child(
                Self::page_counter_label(self.current_page(), self.pages.len()),
            ))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .when_some(background_activity, |this, activity| this.child(activity))
                    .child(self.render_ai_button(dock.ai_panel_open, cx)),
            )
            .into_any_element()
    }
}
