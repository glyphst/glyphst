use super::*;

impl DocumentView {
    pub(super) fn selected_page_indices(&self, source: PageSelectionSource) -> Vec<usize> {
        let selection = match source {
            PageSelectionSource::GridOverview => &self.grid_overview_selection,
            PageSelectionSource::ThumbnailSidebar => &self.thumbnail_sidebar_selection,
        };

        selection
            .iter()
            .copied()
            .filter(|page_idx| *page_idx < self.pages.len())
            .collect()
    }

    pub(super) fn cancel_page_selection(
        &mut self,
        source: PageSelectionSource,
        cx: &mut Context<Self>,
    ) {
        match source {
            PageSelectionSource::GridOverview => self.cancel_grid_overview_selection(cx),
            PageSelectionSource::ThumbnailSidebar => self.cancel_thumbnail_sidebar_selection(cx),
        }
    }

    fn enter_grid_overview_selection(&mut self, cx: &mut Context<Self>) {
        self.grid_overview_selecting = true;
        self.close_context_menu(cx);
        cx.notify();
    }

    fn cancel_grid_overview_selection(&mut self, cx: &mut Context<Self>) {
        let changed = self.grid_overview_selecting || !self.grid_overview_selection.is_empty();
        self.grid_overview_selecting = false;
        self.grid_overview_selection.clear();
        if changed {
            cx.notify();
        }
    }

    fn toggle_grid_overview_page_selection(&mut self, page_idx: usize, cx: &mut Context<Self>) {
        self.grid_overview_cursor = page_idx.min(self.last_page_index());
        if !self.grid_overview_selection.insert(page_idx) {
            self.grid_overview_selection.remove(&page_idx);
        }
        cx.notify();
    }

    fn enter_thumbnail_sidebar_selection(&mut self, cx: &mut Context<Self>) {
        self.thumbnail_sidebar_selecting = true;
        self.close_context_menu(cx);
        cx.notify();
    }

    fn cancel_thumbnail_sidebar_selection(&mut self, cx: &mut Context<Self>) {
        let changed =
            self.thumbnail_sidebar_selecting || !self.thumbnail_sidebar_selection.is_empty();
        self.thumbnail_sidebar_selecting = false;
        self.thumbnail_sidebar_selection.clear();
        if changed {
            cx.notify();
        }
    }

    fn toggle_thumbnail_sidebar_page_selection(&mut self, page_idx: usize, cx: &mut Context<Self>) {
        if !self.thumbnail_sidebar_selection.insert(page_idx) {
            self.thumbnail_sidebar_selection.remove(&page_idx);
        }
        cx.notify();
    }

    pub(super) fn set_thumbnail_sidebar_open(
        &mut self,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.thumbnail_sidebar_open == open {
            return;
        }

        self.thumbnail_sidebar_open = open;
        if open {
            self.close_left_sidebars_except(LeftSidebar::Thumbnails);
            let width = Self::clamp_sidebar_width_for_window(
                self.thumbnail_sidebar_width,
                THUMBNAIL_SIDEBAR_WIDTH,
                window,
            );
            if (self.thumbnail_sidebar_width - width).abs() > f32::EPSILON {
                self.thumbnail_sidebar_width = width;
                self.schedule_sidebar_width_save(cx);
            }
            self.thumbnail_sidebar_current_page =
                self.current_page_index().min(self.last_page_index());
            if !self.pages.is_empty() {
                self.thumbnail_sidebar_scroll
                    .scroll_to_item(self.thumbnail_sidebar_current_page, ScrollStrategy::Center);
            }
        } else {
            self.thumbnail_sidebar_selecting = false;
            self.thumbnail_sidebar_selection.clear();
        }
        cx.notify();
    }

    fn sync_thumbnail_sidebar_current_page(&mut self) {
        if !self.thumbnail_sidebar_open || self.thumbnail_sidebar_selecting || self.pages.is_empty()
        {
            return;
        }

        let sync_scroll_y = if (self.target_scroll_y_doc - self.scroll_y_doc).abs() > f32::EPSILON {
            self.target_scroll_y_doc
        } else {
            self.scroll_y_doc
        };
        let current_page = self
            .page_index_for_scroll_y(sync_scroll_y)
            .min(self.last_page_index());
        if current_page == self.thumbnail_sidebar_current_page {
            return;
        }

        self.thumbnail_sidebar_current_page = current_page;
        self.thumbnail_sidebar_scroll
            .scroll_to_item(current_page, ScrollStrategy::Center);
    }

    pub(super) fn set_grid_overview_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.grid_overview_open == open {
            return;
        }

        self.grid_overview_open = open;
        if open {
            self.region_tool_active = false;
            self.region_drag = None;
            self.selected_region = None;
            self.context_menu_region = false;
            self.grid_overview_cursor = self.current_page_index().min(self.last_page_index());
            self.scroll_grid_overview_cursor(ScrollStrategy::Center);
        } else {
            self.grid_overview_selecting = false;
            self.grid_overview_selection.clear();
        }
        cx.notify();
    }

    /// Compute the available content width for the grid overview by subtracting
    /// open sidebar widths (including their 1px borders) from the window width.
    /// This allows column count to be computed synchronously during render,
    /// eliminating the one-frame delay from the measure-canvas approach.
    fn available_content_width(&self, window: &Window) -> f32 {
        let window_width = window.bounds().size.width.as_f32();
        let mut sidebar_total = 0.0f32;
        if self.thumbnail_sidebar_open {
            sidebar_total += self.thumbnail_sidebar_width + 1.0;
        }
        if self.toc_sidebar_open {
            sidebar_total += self.toc_sidebar_width + 1.0;
        }
        if self.notes_sidebar_open {
            sidebar_total += self.notes_sidebar_width + 1.0;
        }
        if self.ai_panel.is_some() {
            sidebar_total += self.ai_panel_width + 1.0;
        }
        (window_width - sidebar_total).max(0.0)
    }

    fn grid_overview_thumbnail_max_width(zoom: f32) -> f32 {
        GRID_OVERVIEW_BASE_THUMBNAIL_WIDTH * zoom
    }

    fn grid_overview_thumbnail_max_height(zoom: f32) -> f32 {
        GRID_OVERVIEW_BASE_THUMBNAIL_MAX_HEIGHT * zoom
    }

    fn grid_overview_cell_width(zoom: f32) -> f32 {
        Self::grid_overview_thumbnail_max_width(zoom) + GRID_OVERVIEW_CELL_PADDING
    }

    fn grid_overview_row_height(zoom: f32) -> f32 {
        Self::grid_overview_thumbnail_max_height(zoom)
            + GRID_OVERVIEW_CELL_PADDING
            + GRID_OVERVIEW_LABEL_HEIGHT
            + GRID_OVERVIEW_ROW_PADDING_Y
    }

    fn grid_overview_cols_for_width(viewport_width: f32, zoom: f32) -> usize {
        if !viewport_width.is_finite() || viewport_width <= 0.0 {
            return GRID_OVERVIEW_DEFAULT_COLS;
        }

        let cell_width = Self::grid_overview_cell_width(zoom).max(1.0);
        (((viewport_width - GRID_OVERVIEW_CELL_GAP) / (cell_width + GRID_OVERVIEW_CELL_GAP)).floor()
            as usize)
            .max(1)
    }

    fn grid_overview_horizontal_gap(viewport_width: f32, cols: usize, zoom: f32) -> f32 {
        if !viewport_width.is_finite() || viewport_width <= 0.0 {
            return GRID_OVERVIEW_CELL_GAP;
        }

        let cells_width = Self::grid_overview_cell_width(zoom) * cols as f32;
        ((viewport_width - cells_width) / (cols + 1) as f32).max(GRID_OVERVIEW_CELL_GAP)
    }

    pub(super) fn update_grid_overview_cols(
        &mut self,
        viewport_width: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        if !viewport_width.is_finite() || viewport_width <= 0.0 {
            return false;
        }

        let viewport_width_changed = (viewport_width - self.grid_overview_viewport_width).abs()
            > GRID_OVERVIEW_VIEWPORT_WIDTH_EPSILON;
        self.grid_overview_viewport_width = viewport_width;
        let cols = Self::grid_overview_cols_for_width(viewport_width, self.grid_overview_zoom);
        let cols_changed = cols != self.grid_overview_cols;
        if !cols_changed && !viewport_width_changed {
            return false;
        }

        self.grid_overview_cols = cols;
        if cols_changed {
            self.scroll_grid_overview_cursor(ScrollStrategy::Center);
        }
        cx.notify();
        true
    }

    fn thumbnail_sidebar_zoom(width: f32) -> f32 {
        let thumbnail_width = (width
            - THUMBNAIL_SIDEBAR_HORIZONTAL_PADDING * 2.0
            - THUMBNAIL_SIDEBAR_THUMBNAIL_PADDING)
            .clamp(
                THUMBNAIL_SIDEBAR_MIN_THUMBNAIL_WIDTH,
                THUMBNAIL_SIDEBAR_MAX_THUMBNAIL_WIDTH,
            );

        thumbnail_width / GRID_OVERVIEW_BASE_THUMBNAIL_WIDTH
    }

    fn thumbnail_sidebar_row_height(width: f32) -> f32 {
        let zoom = Self::thumbnail_sidebar_zoom(width);

        Self::grid_overview_thumbnail_max_height(zoom)
            + THUMBNAIL_SIDEBAR_THUMBNAIL_PADDING
            + THUMBNAIL_SIDEBAR_LABEL_HEIGHT
            + THUMBNAIL_SIDEBAR_ROW_PADDING_Y
    }

    fn grid_overview_thumbnail_size(&self, page_idx: usize, zoom: f32) -> (f32, f32) {
        let (page_width, page_height) = self.page_dimensions(page_idx);
        let page_width = page_width.max(1.0);
        let page_height = page_height.max(1.0);
        let scale = (Self::grid_overview_thumbnail_max_width(zoom) / page_width)
            .min(Self::grid_overview_thumbnail_max_height(zoom) / page_height)
            .max(0.001);

        (page_width * scale, page_height * scale)
    }

    pub(super) fn render_thumbnail_sidebar(
        &mut self,
        theme: ThemeColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.sync_thumbnail_sidebar_current_page();

        let page_count = self.pages.len();
        let selected_count = self
            .thumbnail_sidebar_selection
            .iter()
            .filter(|page_idx| **page_idx < page_count)
            .count();
        let page_export = self.capabilities.page_export;
        let selecting = page_export && self.thumbnail_sidebar_selecting;
        let zoom = Self::thumbnail_sidebar_zoom(self.thumbnail_sidebar_width);
        let row_height = Self::thumbnail_sidebar_row_height(self.thumbnail_sidebar_width);
        let thumbnail_max_width = Self::grid_overview_thumbnail_max_width(zoom);
        let thumbnail_max_height = Self::grid_overview_thumbnail_max_height(zoom);
        let thumbnail_box_width = thumbnail_max_width + THUMBNAIL_SIDEBAR_THUMBNAIL_PADDING;
        let thumbnail_box_height = thumbnail_max_height + THUMBNAIL_SIDEBAR_THUMBNAIL_PADDING;
        let scroll_handle = self.thumbnail_sidebar_scroll.clone();
        let compact_header =
            selecting && self.thumbnail_sidebar_width < THUMBNAIL_SIDEBAR_COMPACT_HEADER_WIDTH;
        let title = if compact_header {
            String::new()
        } else {
            crate::fl!("pdf-thumbnails-title")
        };

        let toolbar_label = if selecting && selected_count > 0 && !compact_header {
            Some(crate::fl!("selected-count", count = selected_count as i64))
        } else {
            None
        };

        let mut toolbar_actions = div().flex().items_center().gap_2();
        let actions_label = if compact_header {
            "⋯".to_string()
        } else {
            crate::fl!("actions-menu")
        };
        let cancel_label = if compact_header {
            "×".to_string()
        } else {
            crate::fl!("cancel")
        };
        if page_export {
            if selecting && selected_count > 0 {
                toolbar_actions = toolbar_actions
                    .child(Self::page_selection_actions_button(
                        "thumbnail-sidebar-actions",
                        actions_label,
                        PageSelectionSource::ThumbnailSidebar,
                        cx,
                    ))
                    .child(Self::grid_overview_toolbar_button(
                        "thumbnail-sidebar-cancel",
                        cancel_label,
                        |this, _, _, cx| this.cancel_thumbnail_sidebar_selection(cx),
                        cx,
                    ));
            } else if selecting {
                toolbar_actions = toolbar_actions.child(Self::grid_overview_toolbar_button(
                    "thumbnail-sidebar-cancel",
                    cancel_label,
                    |this, _, _, cx| this.cancel_thumbnail_sidebar_selection(cx),
                    cx,
                ));
            } else {
                toolbar_actions = toolbar_actions.child(Self::grid_overview_toolbar_button(
                    "thumbnail-sidebar-select",
                    crate::fl!("select"),
                    |this, _, _, cx| this.enter_thumbnail_sidebar_selection(cx),
                    cx,
                ));
            }
        }

        let header_end = div()
            .flex()
            .items_center()
            .gap_2()
            .when_some(toolbar_label, |this, label| {
                this.child(
                    div()
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(label),
                )
            })
            .child(toolbar_actions);

        let content = div()
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| {
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .when(page_count == 0, |this| {
                        this.child(Self::render_sidebar_empty_message(
                            crate::fl!("pdf-no-pages-available"),
                            theme,
                        ))
                    })
                    .when(page_count > 0, |this| {
                        this.child(
                            uniform_list(
                                "thumbnail-sidebar-list",
                                page_count,
                                cx.processor(
                                    move |this, visible_range: Range<usize>, _window, cx| {
                                        let document_theme =
                                            cx.global::<SettingsStore>().resolved_theme;
                                        let theme = cx.theme().colors;
                                        let current_page =
                                            this.current_page_index().min(this.last_page_index());
                                        let selecting = this.thumbnail_sidebar_selecting;

                                        visible_range
                                            .filter_map(|page_idx| {
                                                this.pages.get(page_idx)?;
                                                let (thumbnail_width, thumbnail_height) =
                                                    this.grid_overview_thumbnail_size(page_idx, zoom);
                                                let current = page_idx == current_page;
                                                let selected = this
                                                    .thumbnail_sidebar_selection
                                                    .contains(&page_idx);
                                                let border_color = if selected || current {
                                                    theme.primary
                                                } else {
                                                    theme.border
                                                };
                                                let label_color = if selected || current {
                                                    theme.primary
                                                } else {
                                                    theme.muted_foreground
                                                };
                                                let thumbnail_view = cx.entity();
                                                let thumbnail = canvas(
                                                    {
                                                        let thumbnail_view =
                                                            thumbnail_view.clone();
                                                        move |bounds, window, cx| {
                                                            thumbnail_view.update(cx, |this, cx| {
                                                                let key = this.render_key(
                                                                    page_idx,
                                                                    bounds,
                                                                    window.scale_factor(),
                                                                );
                                                                this.cached_or_request_render(
                                                                    key, cx,
                                                                )
                                                            })
                                                        }
                                                    },
                                                    move |bounds, image, window, _| {
                                                        window.paint_quad(fill(
                                                            bounds,
                                                            rgb(document_theme.page),
                                                        ));
                                                        if let Some(image) = image {
                                                            let _ = window.paint_image(
                                                                bounds,
                                                                bounds,
                                                                Corners::default(),
                                                                image,
                                                                0,
                                                                false,
                                                            );
                                                        }
                                                    },
                                                )
                                                .w(px(thumbnail_width))
                                                .h(px(thumbnail_height));

                                                Some(
                                                    div()
                                                        .id(("thumbnail-sidebar-page", page_idx))
                                                        .flex()
                                                        .flex_col()
                                                        .items_center()
                                                        .justify_center()
                                                        .w_full()
                                                        .h(px(row_height))
                                                        .px(px(THUMBNAIL_SIDEBAR_HORIZONTAL_PADDING))
                                                        .cursor_pointer()
                                                        .hover(move |style| {
                                                            style.bg(theme.list_hover)
                                                        })
                                                        .on_mouse_down(
                                                            MouseButton::Left,
                                                            cx.listener(move |this, _, _, cx| {
                                                                if this.thumbnail_sidebar_selecting {
                                                                    this.toggle_thumbnail_sidebar_page_selection(
                                                                        page_idx, cx,
                                                                    );
                                                                } else {
                                                                    this.navigate_to_pdf_destination(
                                                                        page_idx, None, None, cx,
                                                                    );
                                                                }
                                                                cx.stop_propagation();
                                                            }),
                                                        )
                                                        .on_mouse_down(
                                                            MouseButton::Right,
                                                            cx.listener(
                                                                move |this,
                                                                      event: &MouseDownEvent,
                                                                      window,
                                                                      cx| {
                                                                    this.context_menu_page =
                                                                        Some(page_idx);
                                                                    this.context_menu_highlight =
                                                                        None;
                                                                    this.context_menu_note = None;
                                                                    this.selected_highlight = None;
                                                                    this.selection = None;
                                                                    this.open_context_menu(
                                                                        event.position,
                                                                        window,
                                                                        cx,
                                                                    );
                                                                },
                                                            ),
                                                        )
                                                        .child(
                                                            div()
                                                                .relative()
                                                                .flex()
                                                                .items_center()
                                                                .justify_center()
                                                                .w(px(thumbnail_box_width))
                                                                .h(px(thumbnail_box_height))
                                                                .border_1()
                                                                .border_color(border_color)
                                                                .bg(if selected {
                                                                    theme
                                                                        .primary
                                                                        .opacity(0x22 as f32 / 255.0)
                                                                } else if current {
                                                                    theme
                                                                        .primary
                                                                        .opacity(0x18 as f32 / 255.0)
                                                                } else {
                                                                    transparent_black()
                                                                })
                                                                .child(thumbnail)
                                                                .when(selecting, |this| {
                                                                    this.child(
                                                                        Checkbox::new((
                                                                            "thumbnail-sidebar-selection",
                                                                            page_idx,
                                                                        ))
                                                                            .xsmall()
                                                                            .checked(selected)
                                                                            .tab_stop(false)
                                                                            .absolute()
                                                                            .top(px(6.0))
                                                                            .right(px(6.0))
                                                                            .p_1()
                                                                            .bg(
                                                                                theme
                                                                                    .popover
                                                                                    .opacity(
                                                                                        0xe8 as f32
                                                                                            / 255.0,
                                                                                    ),
                                                                            )
                                                                            .rounded(
                                                                                cx.theme().radius,
                                                                            ),
                                                                    )
                                                                }),
                                                        )
                                                        .child(
                                                            div()
                                                                .mt_1()
                                                                .h(px(THUMBNAIL_SIDEBAR_LABEL_HEIGHT))
                                                                .text_size(ui_rems(11.0))
                                                                .text_color(label_color)
                                                                .child(format!("{}", page_idx + 1)),
                                                        ),
                                                )
                                            })
                                            .collect::<Vec<_>>()
                                    },
                                ),
                            )
                            .track_scroll(&scroll_handle)
                            .size_full(),
                        )
                        .vertical_scrollbar_for_with_id(
                            &scroll_handle,
                            "thumbnail-sidebar-scrollbar",
                            window,
                            cx,
                        )
                    }),
            );

        let document_view = cx.entity().downgrade();
        Self::render_sidebar_shell(title, header_end, content, theme, document_view)
    }

    pub(super) fn render_grid_overview(
        &mut self,
        theme: ThemeColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Compute available width synchronously from window bounds and sidebar
        // widths, eliminating the one-frame delay of the measure-canvas approach.
        let available_width = self.available_content_width(window);
        if available_width.is_finite() && available_width > 0.0 {
            let new_cols =
                Self::grid_overview_cols_for_width(available_width, self.grid_overview_zoom);
            let cols_changed = new_cols != self.grid_overview_cols;
            self.grid_overview_cols = new_cols;
            self.grid_overview_viewport_width = available_width;
            if cols_changed {
                self.scroll_grid_overview_cursor(ScrollStrategy::Center);
            }
        }

        let cols = self.grid_overview_cols.max(1);
        let rows = self.pages.len().div_ceil(cols);
        let zoom = self.grid_overview_zoom;
        let row_height = Self::grid_overview_row_height(zoom);
        let thumbnail_max_width = Self::grid_overview_thumbnail_max_width(zoom);
        let thumbnail_max_height = Self::grid_overview_thumbnail_max_height(zoom);
        let thumbnail_box_width = thumbnail_max_width + GRID_OVERVIEW_CELL_PADDING;
        let thumbnail_box_height = thumbnail_max_height + GRID_OVERVIEW_CELL_PADDING;
        let horizontal_gap =
            Self::grid_overview_horizontal_gap(self.grid_overview_viewport_width, cols, zoom);
        let scroll_handle = self.grid_overview_scroll.clone();
        let measure_view = cx.entity();
        let page_export = self.capabilities.page_export;

        let measure = canvas(
            {
                let measure_view = measure_view.clone();
                move |bounds, _window, cx| {
                    let width = bounds.size.width.as_f32();
                    measure_view.update(cx, |this, cx| {
                        this.update_grid_overview_cols(width, cx);
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        div()
            .id("grid-overview")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .key_context("GridOverview")
            .flex()
            .flex_col()
            .bg(theme.popover)
            .text_size(ui_rems(12.0))
            .text_color(theme.foreground)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.close_context_menu(cx);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    let page_idx = this.grid_overview_cursor.min(this.last_page_index());
                    this.context_menu_page = Some(page_idx);
                    this.context_menu_highlight = None;
                    this.context_menu_note = None;
                    this.selected_highlight = None;
                    this.open_context_menu(event.position, window, cx);
                }),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                if Self::scroll_wheel_requests_zoom(event) {
                    this.handle_scroll_wheel(event, cx);
                }
                cx.stop_propagation();
            }))
            .child(measure)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(px(GRID_OVERVIEW_HEADER_HEIGHT))
                    .px_3()
                    .border_b(px(1.0))
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_size(ui_rems(13.0))
                            .child(crate::fl!("pdf-page-overview-title")),
                    )
                    .child(
                        {
                            let mut toolbar = div().flex().items_center().gap_2();
                            if page_export {
                                if self.grid_overview_selecting
                                    && !self.grid_overview_selection.is_empty()
                                {
                                    toolbar = toolbar
                                        .child(Self::page_selection_actions_button(
                                            "grid-overview-actions",
                                            crate::fl!("actions-menu"),
                                            PageSelectionSource::GridOverview,
                                            cx,
                                        ))
                                        .child(Self::grid_overview_toolbar_button(
                                            "grid-overview-cancel",
                                            crate::fl!("cancel-selection"),
                                            |this, _, _, cx| {
                                                this.cancel_grid_overview_selection(cx)
                                            },
                                            cx,
                                        ));
                                } else if self.grid_overview_selecting {
                                    toolbar = toolbar.child(Self::grid_overview_toolbar_button(
                                        "grid-overview-cancel",
                                        crate::fl!("cancel"),
                                        |this, _, _, cx| {
                                            this.cancel_grid_overview_selection(cx)
                                        },
                                        cx,
                                    ));
                                } else {
                                    toolbar = toolbar.child(Self::grid_overview_toolbar_button(
                                        "grid-overview-select",
                                        crate::fl!("select"),
                                        |this, _, _, cx| {
                                            this.enter_grid_overview_selection(cx)
                                        },
                                        cx,
                                    ));
                                }
                            }
                            toolbar
                        },
                    ),
            )
            .child(
                div()
                    .id("grid-overview-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        uniform_list(
                            "grid-overview-list",
                            rows,
                            cx.processor(move |this, visible_range: Range<usize>, _window, cx| {
                                // Protect visible grid pages from cache eviction so
                                // thumbnails are not evicted by the pixel-budget LRU.
                                let protect_cols = this.grid_overview_cols.max(1);
                                let protect_page_count = this.pages.len();
                                let protect_doc_id = this.doc_id;
                                let protect_rotation = this.rotation;
                                let protected: Vec<(u64, usize, DocumentRotation)> = visible_range
                                    .clone()
                                    .flat_map(|row_ix| {
                                        let start = row_ix * protect_cols;
                                        let end =
                                            ((row_ix + 1) * protect_cols).min(protect_page_count);
                                        start..end
                                    })
                                    .map(|page_idx| {
                                        (protect_doc_id, page_idx, protect_rotation)
                                    })
                                    .collect();
                                cx.global_mut::<RenderCache>().set_protected_pages(protected);

                                let document_theme =
                                    cx.global::<SettingsStore>().resolved_theme;
                                let theme = cx.theme().colors;
                                let current_page =
                                    this.grid_overview_cursor.min(this.last_page_index());

                                visible_range
                                    .map(|row_ix| {
                                        let mut row = div()
                                            .flex()
                                            .flex_row()
                                            .items_start()
                                            .justify_center()
                                            .gap(px(horizontal_gap))
                                            .w_full()
                                            .h(px(row_height))
                                            .px(px(horizontal_gap))
                                            .py_2();

                                        for col_ix in 0..cols {
                                            let page_idx = row_ix * cols + col_ix;
                                            if page_idx < this.pages.len() {
                                                let (thumbnail_width, thumbnail_height) =
                                                    this.grid_overview_thumbnail_size(page_idx, zoom);
                                                let current = page_idx == current_page;
                                                let selecting = this.grid_overview_selecting;
                                                let selected = this
                                                    .grid_overview_selection
                                                    .contains(&page_idx);
                                                let border_color = if selected || current {
                                                    theme.primary
                                                } else {
                                                    theme.border
                                                };
                                                let label_color = if selected || current {
                                                    theme.primary
                                                } else {
                                                    theme.muted_foreground
                                                };
                                                let thumbnail_view = cx.entity();
                                                let thumbnail = canvas(
                                                    {
                                                        let thumbnail_view = thumbnail_view.clone();
                                                        move |bounds, window, cx| {
                                                            thumbnail_view.update(cx, |this, cx| {
                                                                let key = this.render_key(
                                                                    page_idx,
                                                                    bounds,
                                                                    window.scale_factor(),
                                                                );
                                                                this.cached_or_request_render(
                                                                    key, cx,
                                                                )
                                                            })
                                                        }
                                                    },
                                                    move |bounds, image, window, _| {
                                                        window.paint_quad(fill(
                                                            bounds,
                                                            rgb(document_theme.page),
                                                        ));
                                                        if let Some(image) = image {
                                                            let _ = window.paint_image(
                                                                bounds,
                                                                bounds,
                                                                Corners::default(),
                                                                image,
                                                                0,
                                                                false,
                                                            );
                                                        }
                                                    },
                                                )
                                                .w(px(thumbnail_width))
                                                .h(px(thumbnail_height));

                                                row = row.child(
                                                    div()
                                                        .id(("grid-overview-page", page_idx))
                                                        .flex()
                                                        .flex_none()
                                                        .flex_col()
                                                        .items_center()
                                                        .relative()
                                                        .w(px(thumbnail_box_width))
                                                        .cursor_pointer()
                                                        .hover(move |style| {
                                                            style.bg(theme.list_hover)
                                                        })
                                                        .on_mouse_down(
                                                            MouseButton::Left,
                                                            cx.listener(move |this, _, _, cx| {
                                                                this.close_context_menu(cx);
                                                                if this.grid_overview_selecting {
                                                                    this.toggle_grid_overview_page_selection(
                                                                        page_idx, cx,
                                                                    );
                                                                } else {
                                                                    this.navigate_to_pdf_destination(
                                                                        page_idx, None, None, cx,
                                                                    );
                                                                    this.set_grid_overview_open(
                                                                        false, cx,
                                                                    );
                                                                }
                                                                cx.stop_propagation();
                                                            }),
                                                        )
                                                        .on_mouse_down(
                                                            MouseButton::Right,
                                                            cx.listener(
                                                                move |this,
                                                                      event: &MouseDownEvent,
                                                                      window,
                                                                      cx| {
                                                                    this.grid_overview_cursor =
                                                                        page_idx;
                                                                    this.context_menu_page =
                                                                        Some(page_idx);
                                                                    this.context_menu_highlight =
                                                                        None;
                                                                    this.context_menu_note = None;
                                                                    this.selected_highlight = None;
                                                                    this.open_context_menu(
                                                                        event.position,
                                                                        window,
                                                                        cx,
                                                                    );
                                                                },
                                                            ),
                                                        )
                                                        .child(
                                                            div()
                                                                .relative()
                                                                .flex()
                                                                .items_center()
                                                                .justify_center()
                                                                .w(px(thumbnail_box_width))
                                                                .h(px(thumbnail_box_height))
                                                                .border_1()
                                                                .border_color(border_color)
                                                                .bg(if selected {
                                                                    theme
                                                                        .primary
                                                                        .opacity(0x22 as f32 / 255.0)
                                                                } else if current {
                                                                    theme
                                                                        .primary
                                                                        .opacity(0x18 as f32 / 255.0)
                                                                } else {
                                                                    transparent_black()
                                                                })
                                                                .child(thumbnail)
                                                                .when(selecting, |this| {
                                                                    this.child(
                                                                        Checkbox::new((
                                                                            "grid-overview-selection",
                                                                            page_idx,
                                                                        ))
                                                                            .xsmall()
                                                                            .checked(selected)
                                                                            .tab_stop(false)
                                                                            .absolute()
                                                                            .top(px(6.0))
                                                                            .right(px(6.0))
                                                                            .p_1()
                                                                            .bg(
                                                                                theme
                                                                                    .popover
                                                                                    .opacity(
                                                                                        0xe8 as f32
                                                                                            / 255.0,
                                                                                    ),
                                                                            )
                                                                            .rounded(
                                                                                cx.theme().radius,
                                                                            ),
                                                                    )
                                                                }),
                                                        )
                                                        .child(
                                                            div()
                                                                .mt_1()
                                                                .text_size(ui_rems(11.0))
                                                                .text_color(label_color)
                                                                .child(format!("{}", page_idx + 1)),
                                                        ),
                                                );
                                            } else {
                                                row = row.child(
                                                    div().flex_none().w(px(thumbnail_box_width)),
                                                );
                                            }
                                        }

                                        row
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&scroll_handle)
                        .size_full(),
                    )
                    .vertical_scrollbar_for_with_id(
                        &scroll_handle,
                        "grid-overview-scrollbar",
                        window,
                        cx,
                    ),
            )
    }
}
