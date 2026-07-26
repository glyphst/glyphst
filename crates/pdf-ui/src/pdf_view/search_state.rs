use super::*;

impl PdfView {
    pub(super) fn deploy_search_query(
        &mut self,
        query: SharedString,
        previous: bool,
        cx: &mut Context<Self>,
    ) {
        if query.as_ref() == self.search.query.as_ref() {
            self.search
                .update_reveal_after_indexing(self.text_index_state.is_pending_or_indexing());
            self.navigate_search(previous, cx);
            return;
        }

        self.search.query = query;
        self.search
            .update_reveal_after_indexing(self.text_index_state.is_pending_or_indexing());
        self.recompute_search();
        self.search.active = self.nearest_search_match_index();
        if previous
            && let Some(active) = self.search.active
            && !self.search.matches.is_empty()
        {
            self.search.active = Some(
                active
                    .checked_sub(1)
                    .unwrap_or(self.search.matches.len() - 1),
            );
        }
        self.sync_search_bar(cx);

        if self.search.active.is_some() {
            self.record_search_jump_origin();
            self.scroll_to_active_search_match(cx);
        }

        cx.notify();
    }

    pub(super) fn live_search(&mut self, query: SharedString, cx: &mut Context<Self>) {
        if query.as_ref() == self.search.query.as_ref() {
            return;
        }

        self.search.query = query;
        self.search
            .update_reveal_after_indexing(self.text_index_state.is_pending_or_indexing());
        self.recompute_search();
        self.search.active = self.nearest_search_match_index();
        self.sync_search_bar(cx);

        if self.search.active.is_some() {
            self.record_search_jump_origin();
            self.scroll_to_active_search_match(cx);
        }

        cx.notify();
    }

    pub(super) fn set_search_ignore_case(
        &mut self,
        query: SharedString,
        ignore_case: bool,
        cx: &mut Context<Self>,
    ) {
        if query.as_ref() == self.search.query.as_ref() && ignore_case == self.search.ignore_case {
            return;
        }

        self.search.query = query;
        self.search.ignore_case = ignore_case;
        self.search
            .update_reveal_after_indexing(self.text_index_state.is_pending_or_indexing());
        self.recompute_search();
        self.search.active = self.nearest_search_match_index();
        self.sync_search_bar(cx);

        if self.search.active.is_some() {
            self.record_search_jump_origin();
            self.scroll_to_active_search_match(cx);
        }

        cx.notify();
    }

    pub(super) fn recompute_search(&mut self) {
        self.search.matches.clear();
        self.search.active = None;

        let ignore_case = self.search.ignore_case;
        let needle = if ignore_case {
            self.search.query.as_ref().to_lowercase()
        } else {
            self.search.query.as_ref().to_string()
        };
        if needle.is_empty() || self.text_index_state.is_pending_or_indexing() {
            return;
        }

        for page_idx in 0..self.pages.len() {
            let (haystack, glyph_map) = self.searchable_page_text(page_idx, ignore_case);
            let mut start_byte = 0;

            while let Some(relative_start) = haystack[start_byte..].find(&needle) {
                let match_start = start_byte + relative_start;
                let match_end = match_start + needle.len();
                let start_char = Self::char_index_for_byte(&haystack, match_start);
                let end_char = Self::char_index_for_byte(&haystack, match_end);

                if let Some(range) =
                    Self::glyph_range_for_search_chars(&glyph_map, start_char, end_char)
                {
                    self.search.matches.push(SearchMatch { page_idx, range });
                }

                start_byte = match_end;
            }
        }
    }

    fn searchable_page_text(
        &self,
        page_idx: usize,
        ignore_case: bool,
    ) -> (String, Vec<Option<usize>>) {
        let mut text = String::new();
        let mut glyph_map = Vec::new();
        let mut previous: Option<&Glyph> = None;
        let Some(page) = self.pages.get(page_idx) else {
            return (text, glyph_map);
        };
        let range = page.glyph_range.clone();
        if range.start >= range.end || range.end > self.glyphs.len() {
            return (text, glyph_map);
        }

        for glyph_ix in range {
            let glyph = &self.glyphs[glyph_ix];
            if let Some(previous) = previous {
                if glyph.block_idx != previous.block_idx {
                    Self::push_search_separator(&mut text, &mut glyph_map);
                    Self::push_search_separator(&mut text, &mut glyph_map);
                } else if glyph.line_idx != previous.line_idx || previous.inferred_space_after {
                    Self::push_search_separator(&mut text, &mut glyph_map);
                }
            }

            if ignore_case {
                for ch in glyph.c.to_lowercase() {
                    text.push(ch);
                    glyph_map.push(Some(glyph_ix));
                }
            } else {
                text.push(glyph.c);
                glyph_map.push(Some(glyph_ix));
            }
            previous = Some(glyph);
        }

        (text, glyph_map)
    }

    fn push_search_separator(text: &mut String, glyph_map: &mut Vec<Option<usize>>) {
        text.push('\n');
        glyph_map.push(None);
    }

    fn char_index_for_byte(text: &str, byte_ix: usize) -> usize {
        text[..byte_ix].chars().count()
    }

    fn glyph_range_for_search_chars(
        glyph_map: &[Option<usize>],
        start_char: usize,
        end_char: usize,
    ) -> Option<Range<usize>> {
        if start_char >= end_char || end_char > glyph_map.len() {
            return None;
        }

        let start = glyph_map[start_char..end_char]
            .iter()
            .find_map(|glyph_ix| *glyph_ix)?;
        let end = glyph_map[start_char..end_char]
            .iter()
            .rev()
            .find_map(|glyph_ix| *glyph_ix)?
            + 1;

        (start < end).then_some(start..end)
    }

    pub(super) fn nearest_search_match_index(&self) -> Option<usize> {
        if self.search.matches.is_empty() {
            return None;
        }

        self.search
            .matches
            .iter()
            .enumerate()
            .find_map(|(ix, search_match)| {
                self.search_match_doc_rect(search_match)
                    .and_then(|rect| (rect.y1 >= self.scroll_y_doc).then_some(ix))
            })
            .or(Some(0))
    }

    pub(super) fn navigate_search(&mut self, previous: bool, cx: &mut Context<Self>) {
        let match_count = self.search.matches.len();
        if match_count == 0 {
            self.sync_search_bar(cx);
            cx.notify();
            return;
        }

        let next = match self.search.active {
            Some(active) if previous => active.checked_sub(1).unwrap_or(match_count - 1),
            Some(active) => (active + 1) % match_count,
            None if previous => match_count - 1,
            None => 0,
        };

        self.search.active = Some(next);
        self.sync_search_bar(cx);
        self.record_search_jump_origin();
        self.scroll_to_search_match(next, cx);
        cx.notify();
    }

    pub(super) fn scroll_to_active_search_match(&mut self, cx: &mut Context<Self>) {
        if let Some(active) = self.search.active {
            self.scroll_to_search_match(active, cx);
        }
    }

    fn scroll_to_search_match(&mut self, match_ix: usize, cx: &mut Context<Self>) {
        let Some(search_match) = self.search.matches.get(match_ix) else {
            return;
        };
        let Some(doc_rect) = self.search_match_doc_rect(search_match) else {
            return;
        };

        let page_width = self.pages[search_match.page_idx].width;
        let scale = self.effective_scale();
        let center_x = (doc_rect.x0 + doc_rect.x1) / 2.0;
        let center_y = (doc_rect.y0 + doc_rect.y1) / 2.0;

        self.set_scroll_doc(
            center_x - page_width / 2.0,
            center_y - self.viewport_height / (2.0 * scale),
            cx,
        );
    }

    fn search_match_doc_rect(&self, search_match: &SearchMatch) -> Option<Rect> {
        if search_match.range.start >= search_match.range.end
            || search_match.range.end > self.glyphs.len()
            || search_match.page_idx >= self.pages.len()
        {
            return None;
        }

        let mut rect = None;

        for glyph in &self.glyphs[search_match.range.clone()] {
            if glyph.page_idx != search_match.page_idx {
                continue;
            }

            let glyph_rect = Self::quad_rect(&glyph.quad);
            rect = Some(match rect {
                Some(existing) => Self::union_rect(existing, glyph_rect),
                None => glyph_rect,
            });
        }

        let rect = rect?;
        let page = &self.pages[search_match.page_idx];
        Some(Rect::new(
            rect.x0 - page.bounds.x0,
            page.offset_y + rect.y0 - page.bounds.y0,
            rect.x1 - page.bounds.x0,
            page.offset_y + rect.y1 - page.bounds.y0,
        ))
    }

    pub(super) fn sync_search_bar(&mut self, cx: &mut Context<Self>) {
        if let Some(search_bar) = self.search_bar.clone() {
            let query = self.search.query.clone();
            let active = self.search.active;
            let match_count = self.search.matches.len();
            let ignore_case = self.search.ignore_case;
            let indexing = self.text_index_state.is_pending_or_indexing() && !query.is_empty();
            search_bar.update(cx, |search_bar, cx| {
                search_bar.set_match_status(
                    query.clone(),
                    active,
                    match_count,
                    ignore_case,
                    indexing,
                    cx,
                );
            });
        }
    }
}
