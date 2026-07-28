use super::*;

impl DocumentView {
    pub fn set_dock_snapshot(&mut self, snapshot: DocumentDockSnapshot, cx: &mut Context<Self>) {
        self.dock_snapshot = snapshot;
        if let Some(panel) = &self.ai_panel {
            panel.update(cx, |panel, _| panel.set_width(snapshot.widths.ai));
        }
    }

    pub fn supports_sidebar(&self, sidebar: DocumentSidebar) -> bool {
        match sidebar {
            DocumentSidebar::Thumbnails => self.capabilities.page_overview,
            DocumentSidebar::Toc | DocumentSidebar::Notes => true,
        }
    }

    pub fn render_sidebar(
        &mut self,
        sidebar: DocumentSidebar,
        theme: ThemeColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match sidebar {
            DocumentSidebar::Thumbnails => self
                .render_thumbnail_sidebar(theme, window, cx)
                .into_any_element(),
            DocumentSidebar::Toc => self
                .render_toc_sidebar(theme, window, cx)
                .into_any_element(),
            DocumentSidebar::Notes => {
                self.refresh_notes_sidebar_entries();
                self.render_notes_sidebar(theme, window, cx)
                    .into_any_element()
            }
        }
    }

    pub fn close_sidebar(&mut self, sidebar: DocumentSidebar, cx: &mut Context<Self>) {
        if sidebar == DocumentSidebar::Thumbnails {
            self.cancel_thumbnail_sidebar_selection(cx);
        }
    }

    pub(super) fn render_toc_sidebar(
        &mut self,
        theme: ThemeColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.ensure_toc_sidebar_visible_entries();
        let visible_len = self.toc_sidebar_visible_entry_ixs.len();
        let scroll_handle = self.toc_sidebar_scroll.clone();

        let content = div()
            .id("toc-sidebar-content")
            .debug_selector(|| "toc-sidebar-content".to_string())
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
                            let theme = cx.theme().colors;
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
                                        theme.foreground
                                    } else {
                                        theme.muted_foreground
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
                                                theme.list_active
                                            } else {
                                                transparent_black()
                                            })
                                            .when(navigable, |this| {
                                                this.cursor_pointer()
                                                    .hover(move |style| style.bg(theme.list_hover))
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
                                                    .text_color(theme.muted_foreground)
                                                    .when(has_children, |this| {
                                                        this.cursor_pointer()
                                                        .hover(move |style| {
                                                            style.bg(theme.list_hover)
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
                                                    .text_color(title_color)
                                                    .child(title),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .ml_2()
                                                    .w(px(SIDEBAR_PAGE_NUMBER_WIDTH))
                                                    .text_align(gpui::TextAlign::Right)
                                                    .text_color(theme.muted_foreground)
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
                    .size_full(),
                )
                .vertical_scrollbar_for_with_id(
                    &scroll_handle,
                    "toc-sidebar-scrollbar",
                    window,
                    cx,
                )
            });

        let document_view = cx.entity().downgrade();
        Self::render_sidebar_shell(
            crate::fl!("table-of-contents-title"),
            Self::render_sidebar_count(self.toc.len(), theme),
            content,
            theme,
            document_view,
        )
    }

    pub(super) fn render_notes_sidebar(
        &mut self,
        theme: ThemeColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let visible_len = self.notes_sidebar_entries.len();
        let scroll_handle = self.notes_sidebar_scroll.clone();
        let content = div()
            .id("notes-sidebar-content")
            .debug_selector(|| "notes-sidebar-content".to_string())
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
                            let theme = cx.theme().colors;
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
                                                theme.list_active
                                            } else {
                                                transparent_black()
                                            })
                                            .cursor_pointer()
                                            .hover(move |style| style.bg(theme.list_hover))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    this.navigate_to_note(note_ix, cx);
                                                    cx.stop_propagation();
                                                }),
                                            )
                                            .child(div().flex_none().w(px(8.0)).h(px(8.0)).bg(
                                                match color {
                                                    HighlightColor::Yellow => theme.yellow,
                                                    HighlightColor::Green => theme.green,
                                                    HighlightColor::Blue => theme.blue,
                                                    HighlightColor::Pink => theme.magenta,
                                                    HighlightColor::Orange => theme.warning,
                                                },
                                            ))
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
                                                            .text_color(theme.foreground)
                                                            .child(body),
                                                    )
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .text_size(ui_rems(11.0))
                                                            .text_color(theme.muted_foreground)
                                                            .child(text),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .ml_2()
                                                    .w(px(SIDEBAR_PAGE_NUMBER_WIDTH))
                                                    .text_align(gpui::TextAlign::Right)
                                                    .text_color(theme.muted_foreground)
                                                    .child(format!("{}", page)),
                                            ),
                                    )
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&scroll_handle)
                    .size_full(),
                )
                .vertical_scrollbar_for_with_id(
                    &scroll_handle,
                    "notes-sidebar-scrollbar",
                    window,
                    cx,
                )
            });

        let document_view = cx.entity().downgrade();
        Self::render_sidebar_shell(
            crate::fl!("notes-title"),
            Self::render_sidebar_count(visible_len, theme),
            content,
            theme,
            document_view,
        )
    }
}
