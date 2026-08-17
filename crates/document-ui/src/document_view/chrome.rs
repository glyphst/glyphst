use super::bookmarks::{
    bookmark_ribbon_height, bookmark_ribbon_is_filled, bookmark_ribbon_notch_y,
};
use super::*;

impl DocumentView {
    pub(super) fn render_document_bookmark_ribbon(
        &self,
        theme: ThemeColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current_bookmarked = self.current_bookmark().is_some();
        let now = Instant::now();
        let drag_offset = self.bookmark_drag_offset(now);
        let drag_armed = self.bookmark_drag_armed();
        if self.bookmark_ribbon_animating(now) {
            window.request_animation_frame();
        }

        let ribbon_height = bookmark_ribbon_height(drag_offset);
        let ribbon_filled = bookmark_ribbon_is_filled(current_bookmarked, drag_armed);
        let ribbon_color = if ribbon_filled {
            theme.primary
        } else {
            theme.foreground.opacity(0.72)
        };
        let ribbon_state_selector = if ribbon_filled {
            "document-toolbar-bookmark-ribbon-filled"
        } else {
            "document-toolbar-bookmark-ribbon-outline"
        };

        let ribbon = div()
            .id("document-toolbar-bookmark-ribbon")
            .debug_selector(|| "document-toolbar-bookmark-ribbon".to_string())
            .absolute()
            .top_0()
            .left(px((CONTROL_SIZE - BOOKMARK_RIBBON_WIDTH) / 2.0))
            .w(px(BOOKMARK_RIBBON_WIDTH))
            .h(px(ribbon_height))
            .child(
                div()
                    .id(ribbon_state_selector)
                    .debug_selector(move || ribbon_state_selector.to_string())
                    .size_full()
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                let inset = if ribbon_filled {
                                    0.0
                                } else {
                                    BOOKMARK_RIBBON_STROKE_PX / 2.0
                                };
                                let left = bounds.left() + px(inset);
                                let right = bounds.right() - px(inset);
                                let top = bounds.top() + px(inset);
                                let bottom = bounds.bottom() - px(inset);
                                let center_x = left + (right - left) / 2.0;
                                let notch_y =
                                    top + px(bookmark_ribbon_notch_y((bottom - top).as_f32()));
                                let mut path = if ribbon_filled {
                                    PathBuilder::fill()
                                } else {
                                    PathBuilder::stroke(px(BOOKMARK_RIBBON_STROKE_PX))
                                };
                                path.move_to(point(left, top));
                                path.line_to(point(right, top));
                                path.line_to(point(right, bottom));
                                path.line_to(point(center_x, notch_y));
                                path.line_to(point(left, bottom));
                                path.close();
                                if let Ok(path) = path.build() {
                                    window.paint_path(path, ribbon_color);
                                }
                            },
                        )
                        .size_full(),
                    ),
            );

        let bookmark_button = Button::new("document-toolbar-bookmark")
            .debug_selector(|| "document-toolbar-bookmark-button".to_string())
            .custom(ButtonCustomVariant::new(cx))
            .absolute()
            .top_0()
            .left_0()
            .w(px(CONTROL_SIZE))
            .h(px(DOCUMENT_TOOLBAR_HEIGHT))
            .p_0()
            .cursor_pointer()
            .toggled(current_bookmarked)
            .tooltip(if current_bookmarked {
                crate::fl!("bookmark-remove-current")
            } else {
                crate::fl!("bookmark-add-current")
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.begin_bookmark_drag(event.position, cx);
                    cx.stop_propagation();
                }),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if this.activate_bookmark_button(cx) {
                    cx.stop_propagation();
                }
            }));

        div()
            .id("document-toolbar-bookmark-control")
            .debug_selector(|| "document-toolbar-bookmark-control".to_string())
            .absolute()
            .top_0()
            .right(px(BOOKMARK_RIBBON_RIGHT_INSET))
            .w(px(CONTROL_SIZE))
            .h(px(DOCUMENT_TOOLBAR_HEIGHT))
            .child(ribbon)
            .when(self.document_toolbar_visible, |this| {
                this.child(bookmark_button)
            })
            .into_any_element()
    }

    pub(super) fn render_document_toolbar(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let document_background = rgb(cx.global::<SettingsStore>().resolved_theme.background);

        let toolbar = div()
            .id("document-hover-toolbar")
            .debug_selector(|| "document-hover-toolbar".to_string())
            .flex()
            .items_center()
            .justify_between()
            .w_full()
            .h(px(DOCUMENT_TOOLBAR_HEIGHT))
            .px_2()
            .bg(document_background)
            .text_size(ui_rems(12.0))
            .text_color(theme.muted_foreground)
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
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
                    .px_3()
                    .text_color(theme.muted_foreground)
                    .child(Self::page_counter_label(
                        self.current_page_range(),
                        self.pages.len(),
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .h_full()
                    .items_start()
                    .justify_end(),
            );

        if cx.reduce_motion() {
            toolbar.into_any_element()
        } else {
            glyphst_ui::motion::fade_in("document-hover-toolbar-reveal", toolbar).into_any_element()
        }
    }
}
