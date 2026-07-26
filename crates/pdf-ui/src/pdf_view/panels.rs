use super::*;

impl PdfView {
    pub(super) fn close_all_overlays(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_quick_ask(window, cx);
        self.close_search_bar(window, cx);
        self.close_toc_palette(window, cx);
        self.close_notes_palette(window, cx);
        self.close_history_palette(window, cx);
        self.close_bookmark_palette(window, cx);
        self.close_bookmark_prompt(window, cx);
        self.close_go_to_page_prompt(window, cx);
        self.close_epub_style_panel(cx);
    }

    fn apply_transparency(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = cx.global::<SettingsStore>().settings.window;
        window.set_background_appearance(settings.background_appearance(self.transparency_enabled));
        cx.notify();
    }

    pub(super) fn toggle_quick_ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(quick_ask) = self.quick_ask.clone() {
            self.quick_ask_visible = !self.quick_ask_visible;
            if self.quick_ask_visible {
                quick_ask.focus_handle(cx).focus(window, cx);
            } else {
                self.focus_handle.focus(window, cx);
            }
            cx.notify();
            return;
        }

        self.close_all_overlays(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let document_path = self.path.clone();
        let page_state = self.ai_page_state();
        let context_provider = PdfViewAiContextProvider {
            pdf_view: cx.entity().downgrade(),
        };
        let width = self.viewport_width.max(320.0);
        let quick_ask = cx.new(|cx| {
            AiPanel::new_quick_ask(
                previous_focus_handle.clone(),
                context_provider.clone(),
                document_path.clone(),
                page_state,
                width,
                cx,
            )
        });

        cx.subscribe_in(
            &quick_ask,
            window,
            |this, _, event: &AiPanelEvent, window, cx| match event {
                AiPanelEvent::Dismissed => this.close_quick_ask(window, cx),
                AiPanelEvent::Changed => {}
            },
        )
        .detach();

        quick_ask.focus_handle(cx).focus(window, cx);
        self.quick_ask = Some(quick_ask);
        self.quick_ask_visible = true;
        cx.notify();
    }

    fn close_quick_ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quick_ask_visible = false;
        if self.quick_ask.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn toggle_transparency(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.transparency_enabled = !self.transparency_enabled;
        self.apply_transparency(window, cx);
    }

    pub(super) fn toggle_presentation_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.capabilities.presentation {
            return;
        }
        if let Some(page_idx) = self.presentation_page.take() {
            let scroll_y_doc = self.page_scroll_y_doc(page_idx);
            self.set_scroll_doc(self.scroll_x_doc, scroll_y_doc, cx);
            self.mark_manual_navigation();
            self.focus_handle.focus(window, cx);
            cx.notify();
            return;
        }

        let page_idx = self.current_page_index().min(self.last_page_index());
        self.close_all_overlays(window, cx);
        self.close_context_menu(cx);
        self.close_grid_overview_actions(cx);
        self.close_thumbnail_sidebar_actions(cx);
        self.set_grid_overview_open(false, cx);
        self.panning = None;
        self.dragging = false;
        self.pending_link = None;
        self.pending_selection_anchor = None;
        self.hovered_link = None;
        self.hovered_note = None;
        self.hovered_note_position = None;
        self.presentation_page = Some(page_idx);
        self.focus_handle.focus(window, cx);
        self.schedule_position_save(cx);
        cx.notify();
    }

    pub(super) fn toggle_ai_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai_panel.is_some() {
            self.close_ai_panel(window, cx);
            return;
        }

        self.close_all_overlays(window, cx);
        self.clamp_ai_panel_width(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let document_path = self.path.clone();
        let page_state = self.ai_page_state();
        let context_provider = PdfViewAiContextProvider {
            pdf_view: cx.entity().downgrade(),
        };
        let panel = cx.new(|cx| {
            AiPanel::new(
                previous_focus_handle.clone(),
                context_provider.clone(),
                document_path.clone(),
                page_state,
                self.ai_panel_width,
                cx,
            )
        });

        cx.subscribe_in(
            &panel,
            window,
            |this, _, event: &AiPanelEvent, window, cx| match event {
                AiPanelEvent::Dismissed => this.close_ai_panel(window, cx),
                AiPanelEvent::Changed => {}
            },
        )
        .detach();

        panel.focus_handle(cx).focus(window, cx);
        self.ai_panel = Some(panel);
        cx.notify();
    }

    fn close_ai_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.ai_panel.take() {
            panel.update(cx, |panel, cx| panel.save_current_session(cx));
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_bar.is_some() {
            self.close_search_bar(window, cx);
            return;
        }

        self.close_all_overlays(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let query = self.search.query.clone();
        let active = self.search.active;
        let match_count = self.search.matches.len();
        let ignore_case = self.search.ignore_case;
        let indexing = self.text_index_state.is_pending_or_indexing() && !query.is_empty();
        let search_bar = cx.new(|cx| {
            SearchBar::new(
                previous_focus_handle.clone(),
                SearchBarSnapshot {
                    query: query.clone(),
                    active_match: active,
                    match_count,
                    ignore_case,
                    indexing,
                },
                window,
                cx,
            )
        });

        cx.subscribe_in(
            &search_bar,
            window,
            |this, _, event: &SearchEvent, window, cx| match event {
                SearchEvent::Activate { previous, query } => {
                    this.deploy_search_query(query.clone(), *previous, cx)
                }
                SearchEvent::QueryChanged { query } => this.live_search(query.clone(), cx),
                SearchEvent::IgnoreCaseChanged { query, ignore_case } => {
                    this.set_search_ignore_case(query.clone(), *ignore_case, cx)
                }
                SearchEvent::Dismissed => this.close_search_bar(window, cx),
            },
        )
        .detach();

        search_bar.focus_handle(cx).focus(window, cx);
        self.search_bar = Some(search_bar);
        cx.notify();
    }

    fn close_search_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_bar.take().is_some() {
            self.search.query = "".into();
            self.search.matches.clear();
            self.search.active = None;
            self.search.cancel_reveal_after_indexing();
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn toggle_toc_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.toc_sidebar_open {
            self.toc_sidebar_open = false;
            cx.notify();
            return;
        }

        self.close_left_sidebars_except(LeftSidebar::Toc);
        let width =
            Self::clamp_sidebar_width_for_window(self.toc_sidebar_width, TOC_SIDEBAR_WIDTH, window);
        if (self.toc_sidebar_width - width).abs() > f32::EPSILON {
            self.toc_sidebar_width = width;
            self.schedule_sidebar_width_save(cx);
        }
        self.toc_sidebar_open = true;
        cx.notify();
    }

    pub(super) fn toggle_toc_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.toc_palette.is_some() {
            self.close_toc_palette(window, cx);
            return;
        }

        let entries = self.toc_palette_entries();
        self.close_all_overlays(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let selected_ix = self.current_toc_palette_match_ix(&entries);
        let delegate = TocPaletteDelegate::new(cx.entity().downgrade(), entries);
        let palette = cx.new(|cx| {
            Picker::new_with_selected_ix(previous_focus_handle.clone(), delegate, selected_ix, cx)
        });

        cx.subscribe_in(&palette, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_toc_palette(window, cx);
        })
        .detach();

        palette.focus_handle(cx).focus(window, cx);
        self.toc_palette = Some(palette);
        cx.notify();
    }

    pub(super) fn close_toc_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.toc_palette.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn toggle_notes_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.notes_sidebar_open {
            self.notes_sidebar_open = false;
            cx.notify();
            return;
        }

        self.close_left_sidebars_except(LeftSidebar::Notes);
        self.refresh_notes_sidebar_entries();
        let width = Self::clamp_sidebar_width_for_window(
            self.notes_sidebar_width,
            NOTES_SIDEBAR_WIDTH,
            window,
        );
        if (self.notes_sidebar_width - width).abs() > f32::EPSILON {
            self.notes_sidebar_width = width;
            self.schedule_sidebar_width_save(cx);
        }
        self.notes_sidebar_open = true;
        cx.notify();
    }

    pub(super) fn toggle_thumbnail_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.capabilities.page_overview {
            return;
        }
        self.set_thumbnail_sidebar_open(!self.thumbnail_sidebar_open, window, cx);
    }

    pub(super) fn toggle_grid_overview(&mut self, cx: &mut Context<Self>) {
        if !self.capabilities.page_overview {
            return;
        }
        self.set_grid_overview_open(!self.grid_overview_open, cx);
    }

    pub(super) fn toggle_notes_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.notes_palette.is_some() {
            self.close_notes_palette(window, cx);
            return;
        }

        let entries = self.note_palette_entries();
        self.close_all_overlays(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let delegate = NotesPaletteDelegate::new(cx.entity().downgrade(), entries);
        let palette = cx.new(|cx| Picker::new(previous_focus_handle.clone(), delegate, cx));

        cx.subscribe_in(&palette, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_notes_palette(window, cx);
        })
        .detach();

        palette.focus_handle(cx).focus(window, cx);
        self.notes_palette = Some(palette);
        cx.notify();
    }

    pub(super) fn close_notes_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.notes_palette.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn note_palette_entries(&self) -> Vec<NotePaletteEntry> {
        NotePaletteEntry::for_notes(&self.notes)
    }

    pub(super) fn refresh_notes_sidebar_entries(&mut self) {
        let entries = self.note_palette_entries();
        if let Some(selected) = self.notes_sidebar_selected
            && !entries.iter().any(|entry| entry.id == selected)
        {
            self.notes_sidebar_selected = None;
        }
        self.notes_sidebar_entries = entries;
    }

    pub(super) fn toggle_history_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history_palette.is_some() {
            self.close_history_palette(window, cx);
            return;
        }

        self.close_all_overlays(window, cx);

        let entries = cx.global::<LibraryStore>().history_entries().to_vec();
        let previous_focus_handle = self.focus_handle.clone();
        let delegate = HistoryPaletteDelegate::new(cx.entity().downgrade(), entries);
        let palette = cx.new(|cx| Picker::new(previous_focus_handle.clone(), delegate, cx));

        cx.subscribe_in(&palette, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_history_palette(window, cx);
        })
        .detach();

        palette.focus_handle(cx).focus(window, cx);
        self.history_palette = Some(palette);
        cx.notify();
    }

    pub(super) fn close_history_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history_palette.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn toggle_bookmark_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bookmark_palette.is_some() {
            self.close_bookmark_palette(window, cx);
            return;
        }

        self.close_all_overlays(window, cx);

        let bookmarks = cx.global::<LibraryStore>().bookmarks().to_vec();
        let previous_focus_handle = self.focus_handle.clone();
        let delegate =
            BookmarkPaletteDelegate::new(cx.entity().downgrade(), bookmarks, self.path.clone());
        let palette = cx.new(|cx| Picker::new(previous_focus_handle.clone(), delegate, cx));

        cx.subscribe_in(&palette, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_bookmark_palette(window, cx);
        })
        .detach();

        palette.focus_handle(cx).focus(window, cx);
        self.bookmark_palette = Some(palette);
        cx.notify();
    }

    pub(super) fn close_bookmark_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bookmark_palette.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn toggle_bookmark_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bookmark_prompt.is_some() {
            self.close_bookmark_prompt(window, cx);
            return;
        }

        self.close_all_overlays(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let default_label = self.default_bookmark_label();
        let delegate =
            BookmarkPromptDelegate::new(self.path.clone(), self.current_document_position());
        let prompt = cx.new(|cx| {
            Picker::with_content(previous_focus_handle.clone(), delegate, default_label, cx)
        });

        cx.subscribe_in(&prompt, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_bookmark_prompt(window, cx);
        })
        .detach();

        prompt.focus_handle(cx).focus(window, cx);
        self.bookmark_prompt = Some(prompt);
        cx.notify();
    }

    pub(super) fn close_bookmark_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bookmark_prompt.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn toggle_go_to_page_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.capabilities.reflow {
            return;
        }
        if self.go_to_page_prompt.is_some() {
            self.close_go_to_page_prompt(window, cx);
            return;
        }

        self.close_all_overlays(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let delegate = GoToPagePromptDelegate::new(cx.entity().downgrade(), self.page_count());
        let current_page = self.current_page().to_string().into();
        let prompt = cx.new(|cx| {
            Picker::with_content(previous_focus_handle.clone(), delegate, current_page, cx)
        });

        cx.subscribe_in(&prompt, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_go_to_page_prompt(window, cx);
        })
        .detach();

        prompt.focus_handle(cx).focus(window, cx);
        self.go_to_page_prompt = Some(prompt);
        cx.notify();
    }

    pub(super) fn close_go_to_page_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.go_to_page_prompt.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn default_bookmark_label(&self) -> SharedString {
        let current_page = self.current_page_index();
        self.toc
            .iter()
            .rfind(|entry| entry.navigable && entry.page_idx <= current_page)
            .map(|entry| entry.title.clone())
            .unwrap_or_else(|| crate::fl!("page-title", page = self.current_page() as i64).into())
    }

    fn toc_palette_entries(&self) -> Vec<TocPaletteEntry> {
        self.toc
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.navigable)
            .map(|(entry_ix, entry)| TocPaletteEntry {
                entry_ix,
                title: entry.title.clone(),
                depth: entry.depth,
                page_number: entry.page_idx + 1,
            })
            .collect()
    }

    fn current_toc_palette_match_ix(&self, entries: &[TocPaletteEntry]) -> usize {
        if let Some(selected) = self.toc_sidebar_selected
            && let Some(match_ix) = entries.iter().position(|entry| entry.entry_ix == selected)
        {
            return match_ix;
        }

        let current_page_number = self.current_page_index() + 1;
        entries
            .iter()
            .position(|entry| entry.page_number == current_page_number)
            .or_else(|| {
                entries
                    .iter()
                    .rposition(|entry| entry.page_number < current_page_number)
            })
            .unwrap_or(0)
    }
}
