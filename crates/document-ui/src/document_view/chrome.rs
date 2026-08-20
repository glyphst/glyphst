use super::*;

pub(super) fn document_scroll_percentage(scroll_y: f32, max_scroll_y: f32) -> u8 {
    if !max_scroll_y.is_finite() || max_scroll_y <= f32::EPSILON {
        return 100;
    }
    let scroll_y = if scroll_y.is_finite() { scroll_y } else { 0.0 };
    ((scroll_y / max_scroll_y).clamp(0.0, 1.0) * 100.0).floor() as u8
}

fn document_bookmark_icon(bookmarked: bool) -> IconName {
    if bookmarked {
        IconName::BookmarkFill
    } else {
        IconName::Bookmark
    }
}

impl DocumentView {
    fn document_progress_label(&self, display: DocumentProgressDisplay) -> String {
        match display {
            DocumentProgressDisplay::Pages => {
                Self::page_counter_label(self.current_page_range(), self.pages.len())
            }
            DocumentProgressDisplay::Percentage => format!(
                "{}%",
                document_scroll_percentage(self.scroll_y_doc, self.max_scroll_y_doc())
            ),
        }
    }

    fn set_document_progress_display(
        &mut self,
        display: DocumentProgressDisplay,
        cx: &mut Context<Self>,
    ) {
        if cx
            .global::<SettingsStore>()
            .settings
            .document_view
            .progress_display
            == display
        {
            return;
        }

        let mut settings = cx.global::<SettingsStore>().settings.clone();
        settings.document_view.progress_display = display;
        if let Err(error) = cx.global_mut::<SettingsStore>().set_settings(settings) {
            eprintln!("failed to save document progress display: {error:#}");
            return;
        }
        cx.refresh_windows();
    }

    fn open_document_progress_menu(
        &mut self,
        position: GpuiPoint<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        self.focus_handle.focus(window, cx);
        self.context_menu_position = Some(position);

        let selected = cx
            .global::<SettingsStore>()
            .settings
            .document_view
            .progress_display;
        let view = cx.entity();
        let pages_view = view.clone();
        let percentage_view = view;
        let popup = PopupMenu::build(window, cx, move |menu, _, _| {
            menu.item(
                PopupMenuItem::new(crate::fl!("settings-document-progress-pages"))
                    .checked(selected == DocumentProgressDisplay::Pages)
                    .on_click(move |_, _, cx| {
                        pages_view.update(cx, |this, cx| {
                            this.set_document_progress_display(DocumentProgressDisplay::Pages, cx);
                            this.close_context_menu(cx);
                        });
                    }),
            )
            .item(
                PopupMenuItem::new(crate::fl!("settings-document-progress-percentage"))
                    .checked(selected == DocumentProgressDisplay::Percentage)
                    .on_click(move |_, _, cx| {
                        percentage_view.update(cx, |this, cx| {
                            this.set_document_progress_display(
                                DocumentProgressDisplay::Percentage,
                                cx,
                            );
                            this.close_context_menu(cx);
                        });
                    }),
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

    fn render_document_progress(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let display = cx
            .global::<SettingsStore>()
            .settings
            .document_view
            .progress_display;
        let tooltip = if self.capabilities.reflow {
            crate::fl!("document-progress-read-only-tooltip")
        } else {
            crate::fl!("document-progress-tooltip")
        };

        Button::new("document-footer-progress")
            .debug_selector(|| "document-footer-progress".to_string())
            .small()
            .compact()
            .ghost()
            .label(self.document_progress_label(display))
            .h(ui_rems(CONTROL_SIZE))
            .px_2()
            .tooltip(tooltip)
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.open_document_progress_menu(event.position, window, cx);
                }),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                if !this.capabilities.reflow {
                    this.toggle_go_to_page_prompt(window, cx);
                }
                cx.stop_propagation();
            }))
    }

    fn render_document_bookmark_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current_bookmarked = self.current_bookmark().is_some();
        Button::new("document-footer-bookmark")
            .debug_selector(|| "document-footer-bookmark".to_string())
            .small()
            .compact()
            .ghost()
            .icon(document_bookmark_icon(current_bookmarked))
            .w(ui_rems(CONTROL_SIZE))
            .h(ui_rems(CONTROL_SIZE))
            .tooltip(if current_bookmarked {
                crate::fl!("bookmark-remove-current")
            } else {
                crate::fl!("bookmark-add-current")
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, window, cx| {
                    this.close_context_menu(cx);
                    this.open_current_bookmark_title_editor(window, cx);
                    window.prevent_default();
                    cx.stop_propagation();
                }),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.toggle_current_bookmark(cx);
                cx.stop_propagation();
            }))
    }

    pub(super) fn render_document_footer(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("document-footer")
            .debug_selector(|| "document-footer".to_string())
            .flex_none()
            .w_full()
            .h(px(DOCUMENT_FOOTER_HEIGHT))
            .flex()
            .items_center()
            .justify_between()
            .border_t(px(1.0))
            .border_color(theme.status_bar_border)
            .bg(theme.status_bar)
            .text_size(ui_rems(12.0))
            .text_color(theme.muted_foreground)
            .cursor_default()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .pl_2()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .gap_1()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .when(self.capabilities.two_column_layout, |this| {
                        this.child(self.render_two_column_button(cx))
                    })
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
            .child(
                div()
                    .flex_none()
                    .h_full()
                    .px_2()
                    .flex()
                    .items_center()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .child(self.render_document_progress(cx)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .pr_2()
                    .flex()
                    .items_center()
                    .justify_end()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .child(self.render_document_bookmark_button(cx)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{document_bookmark_icon, document_scroll_percentage};

    #[test]
    fn bookmark_state_uses_outline_and_solid_icons() {
        assert_eq!(document_bookmark_icon(false).path(), "icons/bookmark.svg");
        assert_eq!(
            document_bookmark_icon(true).path(),
            "icons/bookmark-fill.svg"
        );
    }

    #[test]
    fn scroll_percentage_covers_top_middle_end_and_non_scrollable_documents() {
        assert_eq!(document_scroll_percentage(0.0, 100.0), 0);
        assert_eq!(document_scroll_percentage(49.9, 100.0), 49);
        assert_eq!(document_scroll_percentage(100.0, 100.0), 100);
        assert_eq!(document_scroll_percentage(120.0, 100.0), 100);
        assert_eq!(document_scroll_percentage(-20.0, 100.0), 0);
        assert_eq!(document_scroll_percentage(0.0, 0.0), 100);
    }
}
