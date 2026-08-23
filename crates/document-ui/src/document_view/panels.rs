use super::*;

impl DocumentView {
    pub(super) fn close_all_overlays(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_search_bar(window, cx);
        self.close_toc_palette(window, cx);
        self.close_notes_palette(window, cx);
        self.close_history_palette(window, cx);
        self.close_bookmark_palette(window, cx);
        self.close_bookmark_title_editor(window, cx);
        self.close_go_to_page_prompt(window, cx);
        self.close_epub_style_panel(cx);
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
        let regex_enabled = self.search.regex_enabled;
        let regex_error = self.search.regex_error.clone();
        let indexing = regex_error.is_none()
            && self.text_index_state.is_pending_or_indexing()
            && !query.is_empty();
        let search_bar = cx.new(|cx| {
            SearchBar::new(
                previous_focus_handle.clone(),
                SearchBarSnapshot {
                    query: query.clone(),
                    active_match: active,
                    match_count,
                    ignore_case,
                    regex_enabled,
                    regex_error: regex_error.clone(),
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
                SearchEvent::RegexChanged {
                    query,
                    regex_enabled,
                } => this.set_search_regex(query.clone(), *regex_enabled, cx),
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
            self.search.clear_results(self.pages.len());
            self.search.cancel_reveal_after_indexing();
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
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
            Picker::new_with_selected_ix(
                previous_focus_handle.clone(),
                delegate,
                selected_ix,
                window,
                cx,
            )
        });

        cx.subscribe_in(&palette, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_toc_palette(window, cx);
        })
        .detach();

        self.toc_palette = Some(palette);
        cx.notify();
    }

    pub(super) fn close_toc_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.toc_palette.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
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
        let palette = cx.new(|cx| Picker::new(previous_focus_handle.clone(), delegate, window, cx));

        cx.subscribe_in(&palette, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_notes_palette(window, cx);
        })
        .detach();

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
        let palette = cx.new(|cx| Picker::new(previous_focus_handle.clone(), delegate, window, cx));

        cx.subscribe_in(&palette, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_history_palette(window, cx);
        })
        .detach();

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
        let palette = cx.new(|cx| Picker::new(previous_focus_handle.clone(), delegate, window, cx));

        cx.subscribe_in(&palette, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_bookmark_palette(window, cx);
        })
        .detach();

        self.bookmark_palette = Some(palette);
        cx.notify();
    }

    pub(super) fn close_bookmark_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bookmark_palette.take().is_some() {
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
            Picker::with_content(
                previous_focus_handle.clone(),
                delegate,
                current_page,
                window,
                cx,
            )
        });

        cx.subscribe_in(&prompt, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_go_to_page_prompt(window, cx);
        })
        .detach();

        self.go_to_page_prompt = Some(prompt);
        cx.notify();
    }

    pub(super) fn close_go_to_page_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.go_to_page_prompt.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn toc_palette_entries(&self) -> Vec<TocPaletteEntry> {
        self.toc
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.navigable)
            .map(|(entry_ix, entry)| TocPaletteEntry {
                entry_ix,
                title: entry.title.clone(),
                depth: entry.depth,
                page_number: entry.page_idx.map(|page_idx| page_idx + 1),
                bookmarked: entry
                    .page_idx
                    .is_some_and(|page_idx| self.is_page_bookmarked(page_idx)),
                color: entry.color.map(pdf_outline_color),
                italic: entry.italic,
                bold: entry.bold,
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
            .position(|entry| entry.page_number == Some(current_page_number))
            .or_else(|| {
                entries.iter().rposition(|entry| {
                    entry
                        .page_number
                        .is_some_and(|page_number| page_number < current_page_number)
                })
            })
            .unwrap_or(0)
    }
}
