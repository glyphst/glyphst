use super::*;

impl PdfView {
    pub(super) fn default_sidebar_widths() -> SidebarWidths {
        SidebarWidths {
            thumbnails: THUMBNAIL_SIDEBAR_WIDTH,
            toc: TOC_SIDEBAR_WIDTH,
            notes: NOTES_SIDEBAR_WIDTH,
            ai: AI_PANEL_WIDTH,
        }
    }

    pub(super) fn sanitize_sidebar_widths(widths: SidebarWidths) -> SidebarWidths {
        let sanitize = |width: f32, fallback: f32| {
            if width.is_finite() {
                width.max(SIDEBAR_MIN_WIDTH)
            } else {
                fallback
            }
        };

        SidebarWidths {
            thumbnails: sanitize(widths.thumbnails, THUMBNAIL_SIDEBAR_WIDTH),
            toc: sanitize(widths.toc, TOC_SIDEBAR_WIDTH),
            notes: sanitize(widths.notes, NOTES_SIDEBAR_WIDTH),
            ai: sanitize(widths.ai, AI_PANEL_WIDTH),
        }
    }

    fn max_sidebar_width_for_window(window: &Window) -> f32 {
        (window.bounds().size.width.as_f32() * SIDEBAR_MAX_WIDTH_RATIO).max(SIDEBAR_MIN_WIDTH)
    }

    pub(super) fn clamp_sidebar_width_for_window(
        width: f32,
        fallback: f32,
        window: &Window,
    ) -> f32 {
        let width = if width.is_finite() { width } else { fallback };
        width.clamp(
            SIDEBAR_MIN_WIDTH,
            Self::max_sidebar_width_for_window(window),
        )
    }

    pub(super) fn clamp_ai_panel_width(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let width =
            Self::clamp_sidebar_width_for_window(self.ai_panel_width, AI_PANEL_WIDTH, window);
        if (self.ai_panel_width - width).abs() <= f32::EPSILON {
            return;
        }

        self.ai_panel_width = width;
        if let Some(panel) = &self.ai_panel {
            panel.update(cx, |panel, cx| {
                panel.set_width(width);
                cx.notify();
            });
        }
        self.schedule_sidebar_width_save(cx);
    }

    pub(super) fn close_left_sidebars_except(&mut self, keep_open: LeftSidebar) {
        if keep_open != LeftSidebar::Thumbnails {
            self.thumbnail_sidebar_open = false;
            self.thumbnail_sidebar_selecting = false;
            self.thumbnail_sidebar_selection.clear();
            self.thumbnail_sidebar_actions_position = None;
        }
        if keep_open != LeftSidebar::Toc {
            self.toc_sidebar_open = false;
        }
        if keep_open != LeftSidebar::Notes {
            self.notes_sidebar_open = false;
        }
    }

    pub(super) fn render_toc_sidebar(
        &mut self,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.ensure_toc_sidebar_visible_entries();
        let visible_len = self.toc_sidebar_visible_entry_ixs.len();
        let scroll_handle = self.toc_sidebar_scroll.clone();

        let content = div()
            .id("toc-sidebar-content")
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .when(visible_len == 0, |this| {
                this.child(Self::render_sidebar_empty_message(
                    crate::fl!("toc-empty"),
                    theme,
                ))
            })
            .when(visible_len > 0, |this| {
                this.child(
                    uniform_list(
                        "toc-sidebar-list",
                        visible_len,
                        cx.processor(|this, visible_range: Range<usize>, _window, cx| {
                            let theme = cx.global::<SettingsStore>().resolved_theme;
                            let selected = this.toc_sidebar_selected;

                            visible_range
                                .filter_map(|visible_ix| {
                                    let entry_ix = this
                                        .toc_sidebar_visible_entry_ixs
                                        .get(visible_ix)
                                        .copied()?;
                                    let entry = &this.toc[entry_ix];
                                    let collapsed = this.toc_collapsed.contains(&entry_ix);
                                    let current = selected == Some(entry_ix);
                                    let title = entry.title.clone();
                                    let title_color = if entry.navigable {
                                        theme.text
                                    } else {
                                        theme.text_muted
                                    };
                                    let depth = entry.depth;
                                    let has_children = entry.has_children;
                                    let navigable = entry.navigable;
                                    let page_number = entry.page_idx + 1;

                                    Some(
                                        div()
                                            .id(("toc-sidebar-entry", entry_ix))
                                            .flex()
                                            .flex_row()
                                            .w_full()
                                            .items_center()
                                            .h(px(TOC_SIDEBAR_ROW_HEIGHT))
                                            .px_2()
                                            .bg(if current {
                                                rgb(theme.element_selected)
                                            } else {
                                                rgba(0x00000000)
                                            })
                                            .when(navigable, |this| {
                                                this.cursor_pointer()
                                                    .hover(move |style| {
                                                        style.bg(rgb(theme.element_hover))
                                                    })
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        cx.listener(move |this, _, _, cx| {
                                                            this.navigate_to_toc_entry(
                                                                entry_ix, cx,
                                                            );
                                                            cx.stop_propagation();
                                                        }),
                                                    )
                                            })
                                            .child(div().flex_none().w(px(depth as f32 * 14.0)))
                                            .child(
                                                div()
                                                    .id(("toc-sidebar-disclosure", entry_ix))
                                                    .flex()
                                                    .flex_none()
                                                    .items_center()
                                                    .justify_center()
                                                    .w(px(18.0))
                                                    .h(px(24.0))
                                                    .text_color(rgb(theme.text_muted))
                                                    .when(has_children, |this| {
                                                        this.cursor_pointer()
                                                        .hover(move |style| {
                                                            style.bg(rgb(theme.element_hover))
                                                        })
                                                        .on_mouse_down(
                                                            MouseButton::Left,
                                                            cx.listener(move |this, _, _, cx| {
                                                                if !this
                                                                    .toc_collapsed
                                                                    .remove(&entry_ix)
                                                                {
                                                                    this.toc_collapsed
                                                                        .insert(entry_ix);
                                                                }
                                                                this.toc_sidebar_entries_dirty =
                                                                    true;
                                                                cx.stop_propagation();
                                                                cx.notify();
                                                            }),
                                                        )
                                                    })
                                                    .child(if has_children {
                                                        if collapsed { "▸" } else { "▾" }
                                                    } else {
                                                        ""
                                                    }),
                                            )
                                            .child(
                                                div()
                                                    .id(("toc-sidebar-title", entry_ix))
                                                    .flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .items_center()
                                                    .h(px(28.0))
                                                    .px_1()
                                                    .truncate()
                                                    .tooltip(text_tooltip(title.clone()))
                                                    .text_color(rgb(title_color))
                                                    .child(title),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .ml_2()
                                                    .w(px(SIDEBAR_PAGE_NUMBER_WIDTH))
                                                    .text_align(gpui::TextAlign::Right)
                                                    .text_color(rgb(theme.text_muted))
                                                    .child(if navigable {
                                                        format!("{}", page_number)
                                                    } else {
                                                        String::new()
                                                    }),
                                            ),
                                    )
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&scroll_handle)
                    .with_decoration(uniform_list_scrollbar_for(
                        &scroll_handle,
                        "toc-sidebar-scrollbar",
                        window,
                        cx,
                    ))
                    .size_full(),
                )
            });

        let pdf_view = cx.entity().downgrade();
        Self::render_sidebar_shell(
            crate::fl!("table-of-contents-title"),
            self.toc_sidebar_width,
            Self::render_sidebar_count(self.toc.len(), theme),
            content,
            Self::render_sidebar_resize_handle(
                "toc-sidebar-resize-handle",
                DraggedSidebar::Toc,
                false,
                cx,
            ),
            theme,
            pdf_view,
        )
    }

    pub(super) fn render_notes_sidebar(
        &mut self,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let visible_len = self.notes_sidebar_entries.len();
        let scroll_handle = self.notes_sidebar_scroll.clone();
        let content = div()
            .id("notes-sidebar-content")
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .when(visible_len == 0, |this| {
                this.child(Self::render_sidebar_empty_message(
                    crate::fl!("notes-empty"),
                    theme,
                ))
            })
            .when(visible_len > 0, |this| {
                this.child(
                    uniform_list(
                        "notes-sidebar-list",
                        visible_len,
                        cx.processor(|this, visible_range: Range<usize>, _window, cx| {
                            let theme = cx.global::<SettingsStore>().resolved_theme;
                            let selected = this.notes_sidebar_selected;

                            visible_range
                                .filter_map(|visible_ix| {
                                    let entry = this.notes_sidebar_entries.get(visible_ix)?;
                                    let note_ix = entry.note_ix;
                                    let note_id = entry.id;
                                    let body = entry.body.clone();
                                    let text = entry.text.clone();
                                    let page = entry.page;
                                    let color = entry.color;
                                    let current = selected == Some(note_id);

                                    Some(
                                        div()
                                            .id(format!("notes-sidebar-note-{note_id}"))
                                            .flex()
                                            .flex_row()
                                            .w_full()
                                            .items_center()
                                            .h(px(NOTES_SIDEBAR_ROW_HEIGHT))
                                            .px_2()
                                            .gap_2()
                                            .bg(if current {
                                                rgb(theme.element_selected)
                                            } else {
                                                rgba(0x00000000)
                                            })
                                            .cursor_pointer()
                                            .hover(move |style| style.bg(rgb(theme.element_hover)))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    this.navigate_to_note(note_ix, cx);
                                                    cx.stop_propagation();
                                                }),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .w(px(8.0))
                                                    .h(px(8.0))
                                                    .bg(rgba(theme.note_color(color))),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .flex_col()
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .text_size(ui_rems(13.0))
                                                            .text_color(rgb(theme.text))
                                                            .child(body),
                                                    )
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .text_size(ui_rems(11.0))
                                                            .text_color(rgb(theme.text_muted))
                                                            .child(text),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .ml_2()
                                                    .w(px(SIDEBAR_PAGE_NUMBER_WIDTH))
                                                    .text_align(gpui::TextAlign::Right)
                                                    .text_color(rgb(theme.text_muted))
                                                    .child(format!("{}", page)),
                                            ),
                                    )
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&scroll_handle)
                    .with_decoration(uniform_list_scrollbar_for(
                        &scroll_handle,
                        "notes-sidebar-scrollbar",
                        window,
                        cx,
                    ))
                    .size_full(),
                )
            });

        let pdf_view = cx.entity().downgrade();
        Self::render_sidebar_shell(
            crate::fl!("notes-title"),
            self.notes_sidebar_width,
            Self::render_sidebar_count(visible_len, theme),
            content,
            Self::render_sidebar_resize_handle(
                "notes-sidebar-resize-handle",
                DraggedSidebar::Notes,
                false,
                cx,
            ),
            theme,
            pdf_view,
        )
    }
}
