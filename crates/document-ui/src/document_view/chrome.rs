use super::bookmarks::bookmark_ribbon_notch_y;
use super::*;

impl DocumentView {
    pub(super) fn render_document_toolbar(
        &self,
        theme: ThemeColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current_bookmarked = self.current_bookmark().is_some();
        let now = Instant::now();
        let drag_offset = self.bookmark_drag_offset(now);
        let drag_armed = self.bookmark_drag_armed();
        let ribbon_visible = drag_offset > 0.0;
        if self.bookmark_ribbon_animating(now) {
            window.request_animation_frame();
        }

        let bookmark_button = Button::new("document-toolbar-bookmark")
            .small()
            .compact()
            .ghost()
            .icon(IconName::Bookmark)
            .w(ui_rems(CONTROL_SIZE))
            .h(ui_rems(CONTROL_SIZE))
            .selected(!ribbon_visible && current_bookmarked)
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

        let button_top = (DOCUMENT_TOOLBAR_HEIGHT - CONTROL_SIZE) / 2.0 + drag_offset;
        let ribbon_height =
            (DOCUMENT_TOOLBAR_HEIGHT - CONTROL_SIZE) / 2.0 + CONTROL_SIZE + drag_offset;
        let ribbon_color = if drag_armed {
            theme.primary.opacity(0.72)
        } else {
            theme.primary.opacity(0.28)
        };
        let bookmark_control = div()
            .id("document-toolbar-bookmark-control")
            .debug_selector(|| "document-toolbar-bookmark-control".to_string())
            .relative()
            .flex_none()
            .w(ui_rems(CONTROL_SIZE))
            .h_full()
            .when(ribbon_visible, |this| {
                this.child(
                    div()
                        .id("document-toolbar-bookmark-ribbon")
                        .debug_selector(|| "document-toolbar-bookmark-ribbon".to_string())
                        .absolute()
                        .top_0()
                        .left_0()
                        .w(ui_rems(CONTROL_SIZE))
                        .h(px(ribbon_height))
                        .child(
                            canvas(
                                |_, _, _| {},
                                move |bounds, _, window, _| {
                                    let mut path = PathBuilder::fill();
                                    let left = bounds.left();
                                    let right = bounds.right();
                                    let top = bounds.top();
                                    let bottom = bounds.bottom();
                                    let center_x = left + bounds.size.width / 2.0;
                                    let notch_y = top
                                        + px(bookmark_ribbon_notch_y(bounds.size.height.as_f32()));
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
                )
            })
            .child(
                div()
                    .absolute()
                    .top(px(button_top))
                    .left_0()
                    .child(bookmark_button),
            );

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
                    .justify_end()
                    .child(bookmark_control),
            );

        if cx.reduce_motion() {
            toolbar.into_any_element()
        } else {
            glyphst_ui::motion::fade_in("document-hover-toolbar-reveal", toolbar).into_any_element()
        }
    }
}
