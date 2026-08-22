use super::page_render::should_show_page_loading;
use super::*;

impl DocumentView {
    fn page_loading_indicators(&self, color: Hsla, reduce_motion: bool) -> Vec<AnyElement> {
        let indicator_size = px(PAGE_LOADING_INDICATOR_SIZE);
        let viewport_bounds = Bounds::new(
            self.viewport_origin,
            size(px(self.viewport_width), px(self.viewport_height)),
        );
        self.viewport_render_debug
            .iter()
            .filter_map(|(page_idx, viewport)| {
                let has_pending_render = self.pending_renders.iter().any(|pending| {
                    pending.key.page_idx == viewport.requested.page_idx
                        && pending.key.source_revision == viewport.requested.source_revision
                        && pending.key.rotation == viewport.requested.rotation
                        && pending.key.color_transform == viewport.requested.color_transform
                });
                if !should_show_page_loading(has_pending_render, viewport.displayed.is_some()) {
                    return None;
                }

                let bounds = self.page_image_bounds.get(*page_idx)?.as_ref()?;
                let center = bounds.intersect(&viewport_bounds).center();
                let indicator = if reduce_motion {
                    Icon::new(ComponentIconName::Loader)
                        .with_size(indicator_size)
                        .text_color(color)
                        .into_any_element()
                } else {
                    Spinner::new()
                        .with_size(indicator_size)
                        .color(color)
                        .into_any_element()
                };

                Some(
                    deferred(
                        anchored()
                            .position(point(center.x, center.y - indicator_size / 2.0))
                            .anchor(Anchor::TopCenter)
                            .child(
                                div()
                                    .id(("document-page-loading", *page_idx))
                                    .debug_selector({
                                        let page_number = *page_idx + 1;
                                        move || format!("document-page-loading-{page_number}")
                                    })
                                    .size(indicator_size)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(indicator),
                            ),
                    )
                    .with_priority(0)
                    .into_any_element(),
                )
            })
            .collect()
    }
}

impl Render for DocumentView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_inherited_target_language(window, cx);
        if self.zoom_popup_visible
            && (self.grid_overview_open || self.is_presentation_mode() || self.search_bar.is_some())
        {
            self.dismiss_zoom_popup();
        }
        let view = cx.entity();
        let prepaint_view = view.clone();
        let theme = cx.global::<SettingsStore>().resolved_theme;
        let ui_theme = cx.theme().colors;
        let reduce_motion = cx.global::<SettingsStore>().settings.behavior.reduce_motion;
        let page_loading_indicators =
            self.page_loading_indicators(ui_theme.muted_foreground, reduce_motion);
        if !self.diff_flashes.is_empty() {
            window.request_animation_frame();
        }
        if !self.is_presentation_mode() {
            if let Some(offset) = self.document_scroll.take_pending_offset() {
                let scale = self.effective_scale().max(0.001);
                let target = (-offset.y.as_f32() / scale).clamp(0.0, self.max_scroll_y_doc());
                if self.set_scroll_doc_synchronized(self.scroll_x_doc, target, cx) {
                    self.mark_manual_navigation();
                }
            }
            self.step_scroll_animation(window, cx);
        }
        let context_menu = self
            .context_menu_position
            .zip(self.context_menu_popup.clone())
            .map(|(position, popup)| {
                deferred(
                    anchored()
                        .snap_to_window_with_margin(px(8.0))
                        .position(position)
                        .anchor(Anchor::BottomLeft)
                        .child(popup),
                )
                .with_priority(1)
            });
        let region_popover = (!self.grid_overview_open
            && self.context_menu_position.is_none()
            && self.region_drag.is_none())
        .then(|| self.region_popover_position())
        .flatten()
        .map(|position| {
            deferred(
                anchored()
                    .snap_to_window_with_margin(px(8.0))
                    .position(position)
                    .anchor(Anchor::TopRight)
                    .child(Self::render_region_popover(cx)),
            )
            .with_priority(2)
        });
        let epub_style_panel = self.epub_style_panel_position.map(|position| {
            deferred(
                anchored()
                    .snap_to_window_with_margin(px(8.0))
                    .position(position)
                    .anchor(Anchor::TopLeft)
                    .child(self.render_epub_style_panel(ui_theme, cx)),
            )
            .with_priority(3)
        });
        let note_popup = if self.context_menu_position.is_none() && self.note_editor.is_none() {
            self.hovered_note
                .and_then(|id| self.hovered_note_position.zip(self.note_body_for_id(id)))
                .map(|(position, body)| {
                    deferred(
                        anchored()
                            .snap_to_window_with_margin(px(8.0))
                            .position(point(position.x + px(12.0), position.y + px(12.0)))
                            .anchor(Anchor::TopLeft)
                            .child(Self::render_note_popup(body, ui_theme, cx)),
                    )
                    .with_priority(1)
                })
        } else {
            None
        };
        let translation_popup = self.translation.popup.clone().and_then(|state| {
            state.position.map(|position| {
                deferred(
                    anchored()
                        .snap_to_window_with_margin(px(8.0))
                        .position(point(position.x + px(12.0), position.y + px(12.0)))
                        .anchor(Anchor::TopLeft)
                        .child(self.render_translation_card(
                            state,
                            false,
                            cx.theme().colors,
                            window,
                            cx,
                        )),
                )
                .with_priority(2)
            })
        });
        let pinned_translation = self.translation.pinned.clone().map(|state| {
            let corner = cx
                .global::<SettingsStore>()
                .settings
                .translation
                .pinned
                .corner;
            let card = self.render_translation_card(state, true, cx.theme().colors, window, cx);
            let mut container = div().absolute().m(px(16.0));
            container = match corner {
                TranslationBoxCorner::TopLeft => container
                    .top(px(DOCUMENT_TOP_POPUP_CLEARANCE))
                    .left(px(0.0)),
                TranslationBoxCorner::TopRight => container
                    .top(px(DOCUMENT_TOP_POPUP_CLEARANCE))
                    .right(px(0.0)),
                TranslationBoxCorner::BottomLeft => {
                    container.bottom(px(DOCUMENT_FOOTER_HEIGHT)).left(px(0.0))
                }
                TranslationBoxCorner::BottomRight => {
                    container.bottom(px(DOCUMENT_FOOTER_HEIGHT)).right(px(0.0))
                }
            };
            container.child(card)
        });
        self.update_translation_hover_animation(
            cx.global::<SettingsStore>().settings.behavior.reduce_motion,
            window,
        );
        let translation_session_panel = self.render_translation_session_panel(window, cx);
        let translation_hover_tools = self.render_translation_hover_tools(cx);

        div()
            .id("document-view-root")
            .debug_selector(|| "document-view-root".to_string())
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .cursor_text()
            .when(self.region_tool_active, |this| this.cursor_crosshair())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.close_epub_style_panel(cx);
                }),
            )
            .when(
                !self.region_tool_active
                    && (self.hovered_link.is_some() || self.hovered_note.is_some()),
                |this| this.cursor_pointer(),
            )
            .when(self.panning.is_some(), |this| this.cursor_grabbing())
            .key_context(if self.region_tool_active {
                "DocumentView RegionSelection"
            } else if self.grid_overview_open {
                "GridOverview"
            } else if self.is_presentation_mode() {
                "DocumentView PresentationMode"
            } else {
                "DocumentView"
            })
            .track_focus(&self.focus_handle)
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered
                    && this.translation.view_role == TranslationViewRole::Translation
                    && this.translation.pair_active
                    && this.translation.overlay_enabled
                {
                    this.schedule_translation_hover_clear(cx);
                }
            }))
            .on_drag_move(cx.listener(
                |this, event: &DragMoveEvent<TranslationPanelDragToken>, _, cx| {
                    if event.drag(cx).0 == this.doc_id
                        && this.update_translation_panel_drag(event.event.position, cx)
                    {
                        cx.stop_propagation();
                    }
                },
            ))
            .on_action(cx.listener(|this, _: &Undo, _, cx| {
                this.undo_document_edit(cx);
            }))
            .on_action(cx.listener(|this, _: &Redo, _, cx| {
                this.redo_document_edit(cx);
            }))
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| {
                this.copy_selection(cx);
            }))
            .on_action(cx.listener(|this, _: &TranslateSelection, _, cx| {
                this.close_context_menu(cx);
                this.translate_selection(
                    this.selection_bounds().and_then(|(start, end)| {
                        this.highlight_bounds_for_range(start..end + 1)
                            .last()
                            .map(|bounds| {
                                point(bounds.right() + px(4.0), bounds.bottom() + px(4.0))
                            })
                    }),
                    false,
                    cx,
                );
            }))
            .when(self.can_translate_document(), |this| {
                this.on_action(cx.listener(|this, _: &TranslateDocument, _, cx| {
                    this.translate_document(cx);
                }))
            })
            .when(
                self.document_translation_jobs
                    .read(cx)
                    .has_active_session_for_source(&self.path)
                    || self
                        .document_translation_jobs
                        .read(cx)
                        .has_job_for_source(&self.path),
                |this| {
                    this.on_action(cx.listener(|this, _: &CancelDocumentTranslation, _, cx| {
                        this.cancel_document_translation(cx);
                    }))
                },
            )
            .on_action(cx.listener(|this, _: &TogglePinnedTranslation, _, cx| {
                this.toggle_pinned_translation(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRegionSelection, _, cx| {
                this.toggle_region_selection(cx);
            }))
            .on_action(cx.listener(|this, _: &CancelRegionSelection, _, cx| {
                this.cancel_region_selection(cx);
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.select_all();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| {
                this.zoom_in(cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| {
                this.zoom_out(cx);
            }))
            .on_action(cx.listener(|this, _: &ResetZoom, _, cx| {
                this.reset_zoom(cx);
            }))
            .on_action(cx.listener(|this, _: &RotateClockwise, _, cx| {
                this.rotate_clockwise(cx);
            }))
            .on_action(cx.listener(|this, _: &RotateCounterclockwise, _, cx| {
                this.rotate_counterclockwise(cx);
            }))
            .on_action(cx.listener(|this, _: &Reload, _, cx| {
                this.request_reload(cx);
            }))
            .on_action(cx.listener(|this, _: &Search, window, cx| {
                this.toggle_search(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenGoToPage, window, cx| {
                this.toggle_go_to_page_prompt(window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextSearchResult, _, cx| {
                this.navigate_search(false, cx);
            }))
            .on_action(cx.listener(|this, _: &PrevSearchResult, _, cx| {
                this.navigate_search(true, cx);
            }))
            .on_action(cx.listener(|this, _: &NextPage, _, cx| {
                this.next_page(cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousPage, _, cx| {
                this.previous_page(cx);
            }))
            .on_action(cx.listener(|this, _: &NextHalfPage, _, cx| {
                this.next_half_page(cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousHalfPage, _, cx| {
                this.previous_half_page(cx);
            }))
            .on_action(cx.listener(|this, _: &ScrollLineDown, _, cx| {
                this.scroll_line(1.0, cx);
            }))
            .on_action(cx.listener(|this, _: &ScrollLineUp, _, cx| {
                this.scroll_line(-1.0, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToBeginning, _, cx| {
                this.go_to_beginning(cx);
            }))
            .on_action(cx.listener(|this, _: &GoToEnd, _, cx| {
                this.go_to_end(cx);
            }))
            .on_action(cx.listener(|this, _: &GridCursorUp, _, cx| {
                this.grid_cursor_up(cx);
            }))
            .on_action(cx.listener(|this, _: &GridCursorDown, _, cx| {
                this.grid_cursor_down(cx);
            }))
            .on_action(cx.listener(|this, _: &GridCursorLeft, _, cx| {
                this.grid_cursor_left(cx);
            }))
            .on_action(cx.listener(|this, _: &GridCursorRight, _, cx| {
                this.grid_cursor_right(cx);
            }))
            .on_action(cx.listener(|this, _: &JumpBackward, _, cx| {
                this.jump_backward(cx);
            }))
            .on_action(cx.listener(|this, _: &JumpForward, _, cx| {
                this.jump_forward(cx);
            }))
            .on_action(cx.listener(|this, _: &Toc, window, cx| {
                this.toggle_toc_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenNotes, window, cx| {
                this.toggle_notes_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleGridOverview, _, cx| {
                this.toggle_grid_overview(cx);
            }))
            .on_action(cx.listener(|this, _: &GridOverviewConfirm, _, cx| {
                this.confirm_grid_overview_selection(cx);
            }))
            .on_action(cx.listener(|_, _: &ToggleFullScreen, window, _| {
                window.toggle_fullscreen();
            }))
            .on_action(cx.listener(|this, _: &TogglePresentationMode, window, cx| {
                this.toggle_presentation_mode(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleTwoColumnMode, _, cx| {
                this.toggle_two_column_mode(cx);
            }))
            .on_action(cx.listener(|_, _: &OpenSettings, _, cx| {
                cx.emit(DocumentViewEvent::OpenSettings);
            }))
            .on_action(cx.listener(|this, _: &OpenHistory, window, cx| {
                this.toggle_history_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenBookmarks, window, cx| {
                this.toggle_bookmark_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleBookmark, _, cx| {
                this.toggle_current_bookmark(cx);
            }))
            .when(self.capabilities.publisher_styles, |this| {
                this.on_action(cx.listener(|this, _: &TogglePublisherStyles, _, cx| {
                    this.toggle_epub_publisher_styles(cx);
                }))
            })
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                this.handle_scroll_wheel(event, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.close_context_menu(cx);
                    this.focus_handle.focus(window, cx);
                    this.dragging = false;
                    this.pending_link = None;
                    this.pending_selection_anchor = None;

                    if this.region_tool_active {
                        this.selection = None;
                        this.selected_highlight = None;
                        this.selected_region = None;
                        this.region_drag = this
                            .point_in_image(event.position)
                            .then(|| this.screen_to_document(event.position))
                            .flatten()
                            .map(|anchor| RegionDragState {
                                anchor,
                                current: anchor,
                            });
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }

                    if let Some(link_ix) = this.link_at_screen(event.position) {
                        this.selection = None;
                        this.selected_highlight = None;
                        this.pending_link = Some(link_ix);
                        this.set_hovered_link(Some(link_ix), cx);
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }

                    this.set_hovered_link(None, cx);
                    this.set_hovered_note(None, None, cx);
                    if !this.point_in_image(event.position) {
                        this.selection = None;
                        this.selected_highlight = None;
                        cx.notify();
                        return;
                    }

                    let highlight_id = this.highlight_at_screen(event.position);
                    let glyph_ix = this.nearby_glyph_at_screen(event.position);
                    match primary_selection_press(highlight_id, glyph_ix, event.click_count) {
                        PrimarySelectionPress::Word(glyph_ix) => {
                            this.selected_highlight = None;
                            if let Some(range) = this.word_bounds_at_glyph(glyph_ix) {
                                this.selection = Some(Selection {
                                    anchor: range.start,
                                    head: range.end - 1,
                                });
                            } else {
                                this.selection = Some(Selection {
                                    anchor: glyph_ix,
                                    head: glyph_ix,
                                });
                                this.dragging = true;
                            }
                        }
                        PrimarySelectionPress::TextAnchor {
                            glyph_ix,
                            highlight_id,
                        } => {
                            this.selection = None;
                            this.selected_highlight = highlight_id;
                            this.pending_selection_anchor = Some(glyph_ix);
                            this.dragging = true;
                        }
                        PrimarySelectionPress::Highlight(highlight_id) => {
                            this.selection = None;
                            this.selected_highlight = Some(highlight_id);
                        }
                        PrimarySelectionPress::Empty => {
                            this.selection = None;
                            this.selected_highlight = None;
                        }
                    }

                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.close_context_menu(cx);
                    this.focus_handle.focus(window, cx);
                    this.dragging = false;
                    this.pending_link = None;
                    this.pending_selection_anchor = None;
                    this.set_hovered_link(None, cx);
                    this.set_hovered_note(None, None, cx);
                    this.panning = Some(PanState {
                        pointer_origin: event.position,
                        scroll_origin_x: this.scroll_x_doc,
                        scroll_origin_y: this.scroll_y_doc,
                    });
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    if this.search_bar.is_some()
                        || this.toc_palette.is_some()
                        || this.history_palette.is_some()
                        || this.bookmark_palette.is_some()
                        || this.bookmark_title_editor.is_some()
                        || this.go_to_page_prompt.is_some()
                        || this.note_editor.is_some()
                    {
                        window.prevent_default();
                        cx.stop_propagation();
                        return;
                    }

                    let pointer_in_text_selection = this.point_in_text_selection(event.position);
                    this.context_menu_highlight = context_highlight_target(
                        this.highlight_at_screen(event.position),
                        pointer_in_text_selection,
                    );
                    this.context_menu_note = this.note_at_screen(event.position);
                    this.context_menu_page = this.page_at_screen(event.position);
                    if let Some(highlight_id) = this.context_menu_highlight {
                        this.selected_highlight = Some(highlight_id);
                        this.selection = None;
                    } else {
                        this.selected_highlight = None;
                    }
                    this.open_context_menu(event.position, window, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this.update_translation_panel_drag(event.position, cx) {
                    cx.stop_propagation();
                    return;
                }
                if let Some(mut drag) = this.region_drag {
                    if !event.dragging() {
                        this.region_drag = None;
                        cx.notify();
                        return;
                    }
                    if let Some(current) = this.screen_to_document(event.position) {
                        drag.current = current;
                        this.region_drag = Some(drag);
                        cx.notify();
                    }
                    return;
                }

                if let Some(pan) = this.panning {
                    this.set_hovered_note(None, None, cx);
                    let scale = this.effective_scale();
                    let delta_x = (event.position.x - pan.pointer_origin.x).as_f32();
                    let delta_y = (event.position.y - pan.pointer_origin.y).as_f32();
                    if this.set_scroll_doc_synchronized(
                        pan.scroll_origin_x - delta_x / scale,
                        pan.scroll_origin_y - delta_y / scale,
                        cx,
                    ) {
                        this.mark_manual_navigation();
                    }
                    return;
                }

                if !this.dragging {
                    this.update_hovered_link(event.position, cx);
                    this.update_hovered_note(event.position, cx);
                    if this.translation.view_role == TranslationViewRole::Translation
                        && this.translation.pair_active
                        && this.translation.overlay_enabled
                    {
                        if this.translation_hover_interaction_contains(event.position) {
                            this.cancel_translation_hover_clear();
                        } else if let Some(region_id) =
                            this.translation_hover_region_at_screen(event.position)
                        {
                            this.set_hovered_translation_region(region_id, cx);
                        } else {
                            this.schedule_translation_hover_clear(cx);
                        }
                    }
                    return;
                }

                if !event.dragging() {
                    this.dragging = false;
                    this.pending_selection_anchor = None;
                    this.update_hovered_link(event.position, cx);
                    this.update_hovered_note(event.position, cx);
                    if this.translation.view_role == TranslationViewRole::Translation
                        && this.translation.pair_active
                        && this.translation.overlay_enabled
                    {
                        this.schedule_translation_hover_clear(cx);
                    }
                    return;
                }

                this.set_hovered_link(None, cx);
                this.set_hovered_note(None, None, cx);
                if let Some(ix) = this.glyph_at_screen(event.position)
                    && extend_text_selection(
                        &mut this.selection,
                        &mut this.selected_highlight,
                        this.pending_selection_anchor,
                        ix,
                    )
                {
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _, cx| {
                    if this.finish_translation_panel_drag(event.position, cx) {
                        cx.stop_propagation();
                        return;
                    }
                    if let Some(mut drag) = this.region_drag.take() {
                        if let Some(current) = this.screen_to_document(event.position) {
                            drag.current = current;
                        }
                        this.selected_region = this.region_reference_for_drag(drag);
                        this.region_tool_active = false;
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }

                    let pending_link = this.pending_link.take();
                    this.dragging = false;
                    this.pending_selection_anchor = None;
                    if let Some(link_ix) = pending_link {
                        if this.link_at_screen(event.position) == Some(link_ix) {
                            this.activate_link(link_ix, cx);
                            cx.stop_propagation();
                        }
                        cx.notify();
                    }
                    if this.translation.pinned.is_some() && this.selected_text().is_some() {
                        this.translate_selection(None, true, cx);
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _, cx| {
                    if this.finish_translation_panel_drag(event.position, cx) {
                        cx.stop_propagation();
                    }
                    if this.pending_link.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, _, _, cx| {
                    this.panning = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .bg(rgb(theme.background))
                    .child(
                        div()
                            .id("document-viewport")
                            .debug_selector(|| "document-viewport".to_string())
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_hidden()
                            .when(!self.grid_overview_open, |this| {
                                this.child(
                                    canvas(
                                        move |bounds, window, cx| {
                                            prepaint_view.update(cx, |this, cx| {
                                                this.paint_data(
                                                    bounds,
                                                    window.scale_factor(),
                                                    window,
                                                    cx,
                                                )
                                                .expect("failed to render PDF")
                                            })
                                        },
                                        move |_, paint_data, window, _cx| {
                                            for page in paint_data.pages {
                                                window
                                                    .paint_quad(fill(page.bounds, rgb(theme.page)));

                                                if let Some(image) = page.image {
                                                    let _ = window.paint_image(
                                                        page.bounds,
                                                        page.bounds,
                                                        Corners::default(),
                                                        image,
                                                        0,
                                                        false,
                                                    );
                                                }
                                                if let Some(texture) = page.gpu_texture
                                                    && let Err(error) =
                                                        window.paint_external_texture(
                                                            texture.bounds,
                                                            Bounds::new(
                                                                point(0.0, 0.0),
                                                                size(1.0, 1.0),
                                                            ),
                                                            texture.texture.handle().clone(),
                                                            1.0,
                                                            ExternalTextureFilter::Linear,
                                                            ExternalTextureColorSpace::LinearSrgb,
                                                            ExternalTextureAlphaMode::Opaque,
                                                        )
                                                {
                                                    eprintln!(
                                                        "failed to present native PDF GPU page: {error:#}"
                                                    );
                                                }
                                            }

                                            for overlay in paint_data.link_invert_overlays {
                                                if let Err(error) = window
                                                    .paint_external_texture_with_effect(
                                                        overlay.bounds,
                                                        overlay.source_bounds,
                                                        overlay.texture,
                                                        1.0,
                                                        ExternalTextureFilter::Linear,
                                                        ExternalTextureColorSpace::LinearSrgb,
                                                        ExternalTextureAlphaMode::Opaque,
                                                        ExternalTextureColorEffect::InvertRgb,
                                                    )
                                                {
                                                    eprintln!(
                                                        "failed to paint native PDF Link press effect: {error:#}"
                                                    );
                                                }
                                            }

                                            for overlay in paint_data.diff_flash_overlays {
                                                window.paint_quad(fill(
                                                    overlay.bounds,
                                                    ui_theme
                                                        .primary
                                                        .opacity(f32::from(overlay.alpha) / 255.0),
                                                ));
                                            }

                                            for (highlight, color) in paint_data.highlight_overlays
                                            {
                                                window.paint_quad(fill(
                                                    highlight,
                                                    rgba(theme.highlight_color(color)),
                                                ));
                                            }

                                            for (note, color) in paint_data.note_underlines {
                                                let dot = px(3.0);
                                                for segment in
                                                    Self::dotted_underline_segments(note, dot)
                                                {
                                                    window.paint_quad(
                                                        fill(
                                                            segment,
                                                            rgba(theme.note_color(color)),
                                                        )
                                                        .corner_radii(Corners::all(dot / 2.0)),
                                                    );
                                                }
                                            }

                                            for highlight in paint_data.search_highlights {
                                                window.paint_quad(fill(
                                                    highlight,
                                                    rgba(Self::with_alpha(
                                                        theme.match_highlight,
                                                        0x66,
                                                    )),
                                                ));
                                            }

                                            for highlight in paint_data.active_search_highlights {
                                                window.paint_quad(fill(
                                                    highlight,
                                                    ui_theme.primary.opacity(0x99 as f32 / 255.0),
                                                ));
                                            }

                                            for highlight in paint_data.link_highlights {
                                                window.paint_quad(fill(
                                                    highlight,
                                                    ui_theme.primary.opacity(0x33 as f32 / 255.0),
                                                ));
                                            }

                                            for highlight in
                                                paint_data.failed_translation_highlights
                                            {
                                                window.paint_quad(fill(
                                                    highlight,
                                                    ui_theme.warning.opacity(0x22 as f32 / 255.0),
                                                ));
                                                window.paint_quad(fill(
                                                    Bounds::new(
                                                        highlight.origin,
                                                        size(px(2.0), highlight.size.height),
                                                    ),
                                                    ui_theme.warning.opacity(0xdd as f32 / 255.0),
                                                ));
                                            }

                                            for highlight in paint_data.linked_hover_highlights {
                                                let blue = rgb(theme.highlight_blue >> 8);
                                                window.paint_quad(
                                                    fill(
                                                        highlight.bounds,
                                                        blue.opacity(
                                                            (0x38 as f32 / 255.0)
                                                                * highlight.opacity,
                                                        ),
                                                    )
                                                    .corner_radii(Corners::all(px(5.0))),
                                                );
                                                window.paint_quad(
                                                    outline(
                                                        highlight.bounds,
                                                        blue.opacity(
                                                            (0xa8 as f32 / 255.0)
                                                                * highlight.opacity,
                                                        ),
                                                        BorderStyle::Solid,
                                                    )
                                                    .corner_radii(Corners::all(px(5.0))),
                                                );
                                            }

                                            for border in paint_data.selected_highlight_borders {
                                                let color =
                                                    ui_theme.primary.opacity(0xdd as f32 / 255.0);
                                                let thickness = px(1.0);
                                                window.paint_quad(fill(
                                                    Bounds::new(
                                                        border.origin,
                                                        size(border.size.width, thickness),
                                                    ),
                                                    color,
                                                ));
                                                window.paint_quad(fill(
                                                    Bounds::new(
                                                        point(
                                                            border.origin.x,
                                                            border.origin.y + border.size.height
                                                                - thickness,
                                                        ),
                                                        size(border.size.width, thickness),
                                                    ),
                                                    color,
                                                ));
                                                window.paint_quad(fill(
                                                    Bounds::new(
                                                        border.origin,
                                                        size(thickness, border.size.height),
                                                    ),
                                                    color,
                                                ));
                                                window.paint_quad(fill(
                                                    Bounds::new(
                                                        point(
                                                            border.origin.x + border.size.width
                                                                - thickness,
                                                            border.origin.y,
                                                        ),
                                                        size(thickness, border.size.height),
                                                    ),
                                                    color,
                                                ));
                                            }

                                            for highlight in paint_data.selection_highlights {
                                                window.paint_quad(fill(
                                                    highlight,
                                                    rgba(theme.selection_highlight),
                                                ));
                                            }

                                            for region in paint_data.region_overlays {
                                                window.paint_quad(fill(
                                                    region,
                                                    ui_theme.primary.opacity(0x33 as f32 / 255.0),
                                                ));
                                                window.paint_quad(outline(
                                                    region,
                                                    ui_theme.primary.opacity(0xee as f32 / 255.0),
                                                    BorderStyle::Solid,
                                                ));
                                            }
                                        },
                                    )
                                    .size_full(),
                                )
                            })
                            .when(
                                !self.is_presentation_mode() && !self.grid_overview_open,
                                |this| {
                                    this.vertical_scrollbar_for_with_id(
                                        &self.document_scroll,
                                        "document-scrollbar",
                                        window,
                                        cx,
                                    )
                                },
                            ),
                    )
                    .when(self.grid_overview_open, |this| {
                        this.child(self.render_grid_overview(ui_theme, window, cx))
                    }),
            )
            .children(page_loading_indicators)
            .when(!self.is_presentation_mode(), |this| {
                this.child(self.render_document_footer(ui_theme, cx))
            })
            .when(
                self.zoom_popup_visible
                    && !self.grid_overview_open
                    && !self.is_presentation_mode()
                    && self.search_bar.is_none(),
                |this| {
                    this.child(
                        div()
                            .absolute()
                            .top(px(12.0))
                            .right(px(16.0))
                            .child(self.render_zoom_popup(cx)),
                    )
                },
            )
            .when_some(self.search_bar.clone(), |this, search_bar| {
                this.child(
                    div()
                        .absolute()
                        .top(px(12.0))
                        .right(px(16.0))
                        .child(search_bar),
                )
            })
            .when_some(self.toc_palette.clone(), |this, palette| {
                this.child(Self::render_centered_overlay(
                    palette,
                    |this, window, cx| this.close_toc_palette(window, cx),
                    ui_theme,
                    cx,
                ))
            })
            .when_some(translation_popup, |this, popup| this.child(popup))
            .when_some(pinned_translation, |this, pinned| this.child(pinned))
            .when_some(translation_session_panel, |this, panel| this.child(panel))
            .when_some(translation_hover_tools, |this, tools| this.child(tools))
            .when_some(self.notes_palette.clone(), |this, palette| {
                this.child(Self::render_centered_overlay(
                    palette,
                    |this, window, cx| this.close_notes_palette(window, cx),
                    ui_theme,
                    cx,
                ))
            })
            .when_some(self.history_palette.clone(), |this, palette| {
                this.child(Self::render_centered_overlay(
                    palette,
                    |this, window, cx| this.close_history_palette(window, cx),
                    ui_theme,
                    cx,
                ))
            })
            .when_some(self.bookmark_palette.clone(), |this, palette| {
                this.child(Self::render_centered_overlay(
                    palette,
                    |this, window, cx| this.close_bookmark_palette(window, cx),
                    ui_theme,
                    cx,
                ))
            })
            .when_some(self.go_to_page_prompt.clone(), |this, prompt| {
                this.child(Self::render_centered_overlay(
                    prompt,
                    |this, window, cx| this.close_go_to_page_prompt(window, cx),
                    ui_theme,
                    cx,
                ))
            })
            .when_some(self.note_editor.clone(), |this, editor| {
                this.child(Self::render_centered_overlay(
                    editor,
                    |this, window, cx| this.request_close_note_editor(window, cx),
                    ui_theme,
                    cx,
                ))
            })
            .when_some(self.bookmark_title_editor.clone(), |this, editor| {
                this.child(
                    div()
                        .absolute()
                        .bottom(px(DOCUMENT_FOOTER_HEIGHT + 8.0))
                        .left(px(12.0))
                        .right(px(12.0))
                        .flex()
                        .justify_end()
                        .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                            this.close_bookmark_title_editor(window, cx);
                        }))
                        .child(editor),
                )
            })
            .children(note_popup)
            .children(context_menu)
            .children(region_popover)
            .children(epub_style_panel)
            .into_any_element()
    }
}
