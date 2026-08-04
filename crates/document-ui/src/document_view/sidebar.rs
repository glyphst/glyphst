use super::*;

#[derive(Clone)]
struct TocStickyCandidate {
    entry_ix: usize,
    depth: usize,
}

impl StickyCandidate for TocStickyCandidate {
    fn depth(&self) -> usize {
        self.depth
    }
}

#[derive(Clone)]
struct TocSidebarScrollbarHandle {
    scroll_handle: UniformListScrollHandle,
    manual_scroll: Rc<Cell<bool>>,
}

// Only the rendered scrollbar receives this wrapper. Auto-follow scrolls the
// original uniform-list handle, so it cannot accidentally mark itself manual.
impl ScrollbarHandle for TocSidebarScrollbarHandle {
    fn offset(&self) -> GpuiPoint<Pixels> {
        ScrollbarHandle::offset(&self.scroll_handle)
    }

    fn set_offset(&self, offset: GpuiPoint<Pixels>) {
        if ScrollbarHandle::offset(&self.scroll_handle) != offset {
            self.manual_scroll.set(true);
        }
        ScrollbarHandle::set_offset(&self.scroll_handle, offset);
    }

    fn content_size(&self) -> gpui::Size<Pixels> {
        ScrollbarHandle::content_size(&self.scroll_handle)
    }
}

fn toc_ancestor_entry_ixs(toc: &[TocEntry], entry_ix: usize) -> Vec<usize> {
    let mut ancestors = Vec::new();
    let mut parent_ix = toc.get(entry_ix).and_then(|entry| entry.parent_ix);
    let mut remaining = toc.len();

    while let Some(entry_ix) = parent_ix {
        if remaining == 0 {
            break;
        }
        remaining -= 1;
        ancestors.push(entry_ix);
        parent_ix = toc.get(entry_ix).and_then(|entry| entry.parent_ix);
    }

    ancestors.reverse();
    ancestors
}

fn expand_toc_ancestor_entries(
    toc: &[TocEntry],
    collapsed: &mut HashSet<usize>,
    entry_ix: usize,
) -> bool {
    let mut changed = false;
    for ancestor_ix in toc_ancestor_entry_ixs(toc, entry_ix) {
        changed |= collapsed.remove(&ancestor_ix);
    }
    changed
}

fn current_toc_entry_for_anchor(
    toc: &[TocEntry],
    anchor_y: f32,
    mut destination_y: impl FnMut(&TocEntry) -> Option<f32>,
) -> Option<usize> {
    let mut first_navigable = None;
    let mut best = None::<(usize, f32)>;

    for (entry_ix, entry) in toc.iter().enumerate() {
        if !entry.navigable {
            continue;
        }
        first_navigable.get_or_insert(entry_ix);
        let Some(destination_y) = destination_y(entry) else {
            continue;
        };

        // Prefer the later outline entry when destinations are identical.
        if destination_y <= anchor_y && best.is_none_or(|(_, best_y)| destination_y >= best_y) {
            best = Some((entry_ix, destination_y));
        }
    }

    best.map(|(entry_ix, _)| entry_ix).or(first_navigable)
}

fn toc_original_row_is_visible(
    visible_ix: Option<usize>,
    scroll_top: f32,
    viewport_height: f32,
) -> bool {
    let Some(visible_ix) = visible_ix else {
        return false;
    };
    let row_top = visible_ix as f32 * TOC_SIDEBAR_ROW_HEIGHT;
    let row_bottom = row_top + TOC_SIDEBAR_ROW_HEIGHT;
    let viewport_bottom = scroll_top + viewport_height;

    row_bottom > scroll_top && row_top < viewport_bottom
}

fn resume_toc_sidebar_follow_state(follow_current: &mut bool, manual_scroll: &Cell<bool>) {
    *follow_current = true;
    manual_scroll.set(false);
}

impl DocumentView {
    pub fn set_dock_snapshot(&mut self, snapshot: DocumentDockSnapshot, cx: &mut Context<Self>) {
        let thumbnail_cache_changed = self.dock_snapshot.left_sidebar
            == Some(DocumentSidebar::Thumbnails)
            && (snapshot.left_sidebar != Some(DocumentSidebar::Thumbnails)
                || self.dock_snapshot.widths.thumbnails != snapshot.widths.thumbnails);
        self.dock_snapshot = snapshot;
        if thumbnail_cache_changed {
            self.thumbnail_sidebar_render_keys.clear();
            self.update_render_cache_protection(cx);
        }
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
            self.thumbnail_sidebar_render_keys.clear();
            self.update_render_cache_protection(cx);
        }
    }

    pub(super) fn render_toc_sidebar(
        &mut self,
        theme: ThemeColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.update_toc_sidebar_follow_after_manual_scroll();
        self.sync_toc_sidebar_current_entry();
        self.ensure_toc_sidebar_visible_entries();

        let visible_len = self.toc_sidebar_visible_entry_ixs.len();
        let scroll_handle = self.toc_sidebar_scroll.clone();
        let scrollbar_handle = TocSidebarScrollbarHandle {
            scroll_handle: scroll_handle.clone(),
            manual_scroll: self.toc_sidebar_manual_scroll.clone(),
        };
        let show_sticky_entries =
            scroll_handle.is_scrollable() && ScrollbarHandle::offset(&scroll_handle).y < px(0.0);
        let manual_scroll = self.toc_sidebar_manual_scroll.clone();

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

                            visible_range
                                .filter_map(|visible_ix| {
                                    let entry_ix = this
                                        .toc_sidebar_visible_entry_ixs
                                        .get(visible_ix)
                                        .copied()?;
                                    this.render_toc_sidebar_entry(entry_ix, false, false, theme, cx)
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .when(show_sticky_entries, |list| {
                        list.with_decoration(sticky_items(
                            cx.entity(),
                            |this, visible_range, _, _| {
                                visible_range
                                    .filter_map(|visible_ix| {
                                        let entry_ix = this
                                            .toc_sidebar_visible_entry_ixs
                                            .get(visible_ix)
                                            .copied()?;
                                        let depth = this.toc.get(entry_ix)?.depth;
                                        Some(TocStickyCandidate { entry_ix, depth })
                                    })
                                    .collect::<SmallVec<[_; 8]>>()
                            },
                            |this, candidate, _, cx| {
                                let theme = cx.theme().colors;
                                let ancestor_ixs = this.toc_ancestor_entry_ixs(candidate.entry_ix);
                                let last_ix = ancestor_ixs.len().saturating_sub(1);

                                ancestor_ixs
                                    .into_iter()
                                    .enumerate()
                                    .filter_map(|(ix, entry_ix)| {
                                        this.render_toc_sidebar_entry(
                                            entry_ix,
                                            true,
                                            ix == last_ix,
                                            theme,
                                            cx,
                                        )
                                    })
                                    .collect::<SmallVec<[_; 8]>>()
                            },
                        ))
                    })
                    .on_scroll_wheel(move |_, _, _| {
                        manual_scroll.set(true);
                    })
                    .track_scroll(&scroll_handle)
                    .size_full(),
                )
                .vertical_scrollbar_for_with_id(
                    &scrollbar_handle,
                    "toc-sidebar-scrollbar",
                    window,
                    cx,
                )
            });

        let locate_enabled = self.toc_sidebar_selected.is_some();
        let following = locate_enabled && self.toc_sidebar_follow_current;
        let locate_tooltip = if following {
            crate::fl!("toc-following-current")
        } else {
            crate::fl!("toc-locate-current")
        };
        let header_end = div()
            .flex()
            .items_center()
            .when(!self.toc.is_empty(), |this| {
                this.child(
                    toolbar_icon_button("toc-sidebar-locate-current", IconName::LocateFixed)
                        .disabled(!locate_enabled)
                        .tooltip(locate_tooltip)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.locate_current_toc_entry(cx);
                            cx.stop_propagation();
                        })),
                )
            });

        let document_view = cx.entity().downgrade();
        Self::render_sidebar_shell(
            crate::fl!("table-of-contents-title"),
            header_end,
            content,
            theme,
            document_view,
        )
    }

    fn render_toc_sidebar_entry(
        &self,
        entry_ix: usize,
        sticky: bool,
        sticky_shadow: bool,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let entry = self.toc.get(entry_ix)?;
        let collapsed = self.toc_collapsed.contains(&entry_ix);
        let current = self.toc_sidebar_selected == Some(entry_ix);
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
        let row_id = if sticky {
            "toc-sidebar-sticky-entry"
        } else {
            "toc-sidebar-entry"
        };
        let disclosure_id = if sticky {
            "toc-sidebar-sticky-disclosure"
        } else {
            "toc-sidebar-disclosure"
        };
        let title_id = if sticky {
            "toc-sidebar-sticky-title"
        } else {
            "toc-sidebar-title"
        };
        let background = if sticky {
            if current {
                theme.popover.blend(theme.list_active)
            } else {
                theme.popover
            }
        } else if current {
            theme.list_active
        } else {
            transparent_black()
        };
        let hover_background = if sticky {
            theme.popover.blend(theme.list_hover)
        } else {
            theme.list_hover
        };

        let row = div()
            .id((row_id, entry_ix))
            .debug_selector(move || format!("{row_id}-{entry_ix}"))
            .relative()
            .flex()
            .flex_row()
            .size_full()
            .items_center()
            .px_2()
            .rounded(cx.theme().radius)
            .bg(background)
            .when(navigable, |this| {
                this.cursor_pointer()
                    .hover(move |style| style.bg(hover_background))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.navigate_to_toc_sidebar_entry(entry_ix, cx);
                            cx.stop_propagation();
                        }),
                    )
            })
            .child(div().flex_none().w(px(depth as f32 * 14.0)))
            .child(
                div()
                    .id((disclosure_id, entry_ix))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .w(px(18.0))
                    .h(px(24.0))
                    .rounded(cx.theme().radius / 2.0)
                    .text_color(theme.muted_foreground)
                    .when(has_children && !sticky, |this| {
                        this.cursor_pointer()
                            .hover(move |style| style.bg(theme.list_hover))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    if !this.toc_collapsed.remove(&entry_ix) {
                                        this.toc_collapsed.insert(entry_ix);
                                    }
                                    this.toc_sidebar_entries_dirty = true;
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
                    .id((title_id, entry_ix))
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
            );

        Some(
            div()
                .relative()
                .w_full()
                .h(px(TOC_SIDEBAR_ROW_HEIGHT))
                .px_1()
                .child(row)
                .when(sticky_shadow, |this| {
                    this.child(
                        div()
                            .absolute()
                            .left_0()
                            .bottom(px(-6.0))
                            .h(px(6.0))
                            .w_full()
                            .bg(linear_gradient(
                                0.0,
                                linear_color_stop(hsla(0.0, 0.0, 0.0, 0.10), 1.0),
                                linear_color_stop(hsla(0.0, 0.0, 0.0, 0.0), 0.0),
                            )),
                    )
                })
                .into_any_element(),
        )
    }

    fn toc_ancestor_entry_ixs(&self, entry_ix: usize) -> Vec<usize> {
        toc_ancestor_entry_ixs(&self.toc, entry_ix)
    }

    fn expand_toc_entry_ancestors(&mut self, entry_ix: usize) -> bool {
        let changed = expand_toc_ancestor_entries(&self.toc, &mut self.toc_collapsed, entry_ix);
        if changed {
            self.toc_sidebar_entries_dirty = true;
        }
        changed
    }

    fn reveal_toc_sidebar_entry(&mut self, entry_ix: usize, strict: bool) {
        self.expand_toc_entry_ancestors(entry_ix);
        self.ensure_toc_sidebar_visible_entries();

        let Some(visible_ix) = self
            .toc_sidebar_visible_entry_ixs
            .iter()
            .position(|candidate_ix| *candidate_ix == entry_ix)
        else {
            return;
        };
        let sticky_offset = self.toc_ancestor_entry_ixs(entry_ix).len();
        if strict {
            self.toc_sidebar_scroll.scroll_to_item_strict_with_offset(
                visible_ix,
                ScrollStrategy::Center,
                sticky_offset,
            );
        } else {
            self.toc_sidebar_scroll.scroll_to_item_with_offset(
                visible_ix,
                ScrollStrategy::Center,
                sticky_offset,
            );
        }
    }

    fn toc_viewport_anchor_y_doc(&self) -> Option<f32> {
        if let Some(page_idx) = self.presentation_page_index() {
            let page = self.pages.get(page_idx)?;
            let (_, page_height) = self.page_dimensions(page_idx);
            return Some(page.offset_y + page_height / 2.0);
        }

        if self.pages.is_empty() {
            return None;
        }
        // Match thumbnail highlighting by tracking the target scroll while an
        // animated document scroll is still settling.
        let scroll_y = if (self.target_scroll_y_doc - self.scroll_y_doc).abs() > f32::EPSILON {
            self.target_scroll_y_doc
        } else {
            self.scroll_y_doc
        };
        let viewport_height_doc = self.viewport_height / self.effective_scale().max(0.001);
        Some(scroll_y + viewport_height_doc / 2.0)
    }

    fn current_toc_entry_ix(&self) -> Option<usize> {
        let anchor_y = self.toc_viewport_anchor_y_doc()?;

        current_toc_entry_for_anchor(&self.toc, anchor_y, |entry| {
            self.pdf_destination_scroll_position(
                entry.page_idx,
                entry.target_top,
                entry.target_left,
            )
            .map(|(_, destination_y)| destination_y)
        })
    }

    fn sync_toc_sidebar_current_entry(&mut self) {
        let current = self.current_toc_entry_ix();
        let changed = self.toc_sidebar_selected != current;
        self.toc_sidebar_selected = current;

        if !self.toc_sidebar_follow_current {
            return;
        }
        let Some(entry_ix) = current else {
            return;
        };
        let expanded = self.expand_toc_entry_ancestors(entry_ix);
        if changed || expanded {
            self.reveal_toc_sidebar_entry(entry_ix, false);
        }
    }

    fn resume_toc_sidebar_follow(&mut self, entry_ix: Option<usize>) {
        self.toc_sidebar_selected = entry_ix;
        resume_toc_sidebar_follow_state(
            &mut self.toc_sidebar_follow_current,
            &self.toc_sidebar_manual_scroll,
        );
        if let Some(entry_ix) = entry_ix {
            self.reveal_toc_sidebar_entry(entry_ix, true);
        }
    }

    fn navigate_to_toc_sidebar_entry(&mut self, entry_ix: usize, cx: &mut Context<Self>) {
        if !self.toc.get(entry_ix).is_some_and(|entry| entry.navigable) {
            return;
        }

        self.navigate_to_toc_entry(entry_ix, cx);
        self.resume_toc_sidebar_follow(Some(entry_ix));
        cx.notify();
    }

    fn locate_current_toc_entry(&mut self, cx: &mut Context<Self>) {
        let current = self.current_toc_entry_ix();
        self.resume_toc_sidebar_follow(current);
        cx.notify();
    }

    fn update_toc_sidebar_follow_after_manual_scroll(&mut self) {
        let offset = ScrollbarHandle::offset(&self.toc_sidebar_scroll);
        let manual_scroll = self.toc_sidebar_manual_scroll.replace(false);
        let offset_changed = offset.y != self.toc_sidebar_last_scroll_offset.y;
        self.toc_sidebar_last_scroll_offset = offset;

        if !manual_scroll || !offset_changed || !self.toc_sidebar_follow_current {
            return;
        }

        let Some(selected_entry_ix) = self.toc_sidebar_selected else {
            return;
        };
        let selected_visible_ix = self
            .toc_sidebar_visible_entry_ixs
            .iter()
            .position(|entry_ix| *entry_ix == selected_entry_ix);

        // UniformList's `item` size is the clipped list viewport; `contents`
        // is the full virtualized content extent.
        let viewport_height = self
            .toc_sidebar_scroll
            .0
            .borrow()
            .last_item_size
            .map(|size| size.item.height.as_f32())
            .unwrap_or(0.0);
        if viewport_height <= 0.0 {
            return;
        }

        let scroll_top = (-offset.y.as_f32()).max(0.0);
        if !toc_original_row_is_visible(selected_visible_ix, scroll_top, viewport_height) {
            self.toc_sidebar_follow_current = false;
        }
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

                                    let row = div()
                                        .id(format!("notes-sidebar-note-{note_id}"))
                                        .flex()
                                        .flex_row()
                                        .size_full()
                                        .items_center()
                                        .px_2()
                                        .gap_2()
                                        .rounded(cx.theme().radius)
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
                                        .child(
                                            div()
                                                .flex_none()
                                                .w(px(8.0))
                                                .h(px(8.0))
                                                .rounded_full()
                                                .bg(match color {
                                                    HighlightColor::Yellow => theme.yellow,
                                                    HighlightColor::Green => theme.green,
                                                    HighlightColor::Blue => theme.blue,
                                                    HighlightColor::Pink => theme.magenta,
                                                    HighlightColor::Orange => theme.warning,
                                                }),
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
                                        );

                                    Some(
                                        div()
                                            .h(px(NOTES_SIDEBAR_ROW_HEIGHT))
                                            .w_full()
                                            .px_1()
                                            .py_0p5()
                                            .child(row),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn toc_entry(parent_ix: Option<usize>, depth: usize, destination_y: f32) -> TocEntry {
        TocEntry {
            title: String::new().into(),
            depth,
            parent_ix,
            has_children: false,
            page_idx: 0,
            target_top: Some(destination_y),
            target_left: None,
            navigable: true,
        }
    }

    #[test]
    fn toc_ancestor_chain_is_root_first_and_has_no_depth_cap() {
        let toc = (0usize..12)
            .map(|depth| {
                toc_entry(
                    depth.checked_sub(1),
                    depth,
                    depth as f32 * TOC_SIDEBAR_ROW_HEIGHT,
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            toc_ancestor_entry_ixs(&toc, 11),
            (0..11).collect::<Vec<_>>()
        );
    }

    #[test]
    fn current_toc_entry_uses_exact_destination_and_later_ties() {
        let mut toc = vec![
            toc_entry(None, 0, 10.0),
            toc_entry(None, 0, 20.0),
            toc_entry(None, 0, 90.0),
            toc_entry(None, 0, 90.0),
        ];
        toc[0].navigable = false;

        let destination_y = |entry: &TocEntry| entry.target_top;
        assert_eq!(
            current_toc_entry_for_anchor(&toc, 89.0, destination_y),
            Some(1)
        );
        assert_eq!(
            current_toc_entry_for_anchor(&toc, 90.0, destination_y),
            Some(3)
        );
        assert_eq!(
            current_toc_entry_for_anchor(&toc, 5.0, destination_y),
            Some(1)
        );
    }

    #[test]
    fn following_current_entry_expands_only_its_ancestors() {
        let toc = vec![
            toc_entry(None, 0, 0.0),
            toc_entry(Some(0), 1, 20.0),
            toc_entry(Some(1), 2, 40.0),
            toc_entry(None, 0, 60.0),
        ];
        let mut collapsed = HashSet::from([0, 1, 3]);

        assert!(expand_toc_ancestor_entries(&toc, &mut collapsed, 2));
        assert_eq!(collapsed, HashSet::from([3]));
        assert!(!expand_toc_ancestor_entries(&toc, &mut collapsed, 2));
    }

    #[test]
    fn scrollbar_adapter_marks_only_observed_offsets_as_manual() {
        let scroll_handle = UniformListScrollHandle::new();
        let manual_scroll = Rc::new(Cell::new(false));
        let scrollbar_handle = TocSidebarScrollbarHandle {
            scroll_handle: scroll_handle.clone(),
            manual_scroll: manual_scroll.clone(),
        };

        ScrollbarHandle::set_offset(&scroll_handle, point(px(0.0), px(-10.0)));
        assert!(!manual_scroll.get());

        ScrollbarHandle::set_offset(&scrollbar_handle, point(px(0.0), px(-20.0)));
        assert!(manual_scroll.get());
    }

    #[test]
    fn resuming_follow_clears_manual_scroll_state() {
        let mut follow_current = false;
        let manual_scroll = Cell::new(true);

        resume_toc_sidebar_follow_state(&mut follow_current, &manual_scroll);

        assert!(follow_current);
        assert!(!manual_scroll.get());
    }

    #[test]
    fn manual_scroll_keeps_following_while_original_row_is_partly_visible() {
        let row_top = TOC_SIDEBAR_ROW_HEIGHT * 2.0;
        let row_bottom = row_top + TOC_SIDEBAR_ROW_HEIGHT;

        assert!(toc_original_row_is_visible(
            Some(2),
            row_bottom - 1.0,
            TOC_SIDEBAR_ROW_HEIGHT,
        ));
        assert!(!toc_original_row_is_visible(
            Some(2),
            row_bottom,
            TOC_SIDEBAR_ROW_HEIGHT,
        ));
        assert!(toc_original_row_is_visible(
            Some(2),
            row_top - TOC_SIDEBAR_ROW_HEIGHT + 1.0,
            TOC_SIDEBAR_ROW_HEIGHT,
        ));
        assert!(!toc_original_row_is_visible(
            Some(2),
            row_top - TOC_SIDEBAR_ROW_HEIGHT,
            TOC_SIDEBAR_ROW_HEIGHT,
        ));
        assert!(!toc_original_row_is_visible(
            None,
            0.0,
            TOC_SIDEBAR_ROW_HEIGHT,
        ));
    }
}
