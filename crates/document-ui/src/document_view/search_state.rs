use super::*;
use regex::{Regex, RegexBuilder};

impl SearchTextIndex {
    fn push(&mut self, ch: char, glyph_ix: Option<usize>) {
        self.mapping.push(SearchTextMapping {
            byte_offset: self.text.len(),
            glyph_ix,
        });
        self.text.push(ch);
    }

    fn push_separator(&mut self) {
        self.push('\n', None);
    }

    fn glyph_range_for_bytes(&self, range: Range<usize>) -> Option<Range<usize>> {
        if range.start >= range.end || range.end > self.text.len() {
            return None;
        }

        let start_mapping = self
            .mapping
            .partition_point(|mapping| mapping.byte_offset < range.start);
        let end_mapping = self
            .mapping
            .partition_point(|mapping| mapping.byte_offset < range.end);
        let mapping = self.mapping.get(start_mapping..end_mapping)?;
        let start = mapping.iter().find_map(|mapping| mapping.glyph_ix)?;
        let end = mapping.iter().rev().find_map(|mapping| mapping.glyph_ix)? + 1;

        (start < end).then_some(start..end)
    }
}

fn regex_glyph_ranges(regex: &Regex, search_text: &SearchTextIndex) -> Vec<Range<usize>> {
    regex
        .find_iter(&search_text.text)
        .filter(|matched| matched.start() < matched.end())
        .filter_map(|matched| search_text.glyph_range_for_bytes(matched.start()..matched.end()))
        .collect()
}

fn search_match_page_fragments(
    pages: &[DocumentPage],
    start_page_idx: usize,
    end_page_idx: usize,
    range: &Range<usize>,
) -> Option<Vec<(usize, Range<usize>)>> {
    if start_page_idx > end_page_idx || end_page_idx >= pages.len() {
        return None;
    }

    let fragments = pages[start_page_idx..=end_page_idx]
        .iter()
        .enumerate()
        .filter_map(|(offset, page)| {
            let fragment =
                range.start.max(page.glyph_range.start)..range.end.min(page.glyph_range.end);
            (fragment.start < fragment.end).then_some((start_page_idx + offset, fragment))
        })
        .collect::<Vec<_>>();
    (!fragments.is_empty()).then_some(fragments)
}

impl DocumentView {
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

    pub(super) fn set_search_regex(
        &mut self,
        query: SharedString,
        regex_enabled: bool,
        cx: &mut Context<Self>,
    ) {
        if query.as_ref() == self.search.query.as_ref()
            && regex_enabled == self.search.regex_enabled
        {
            return;
        }

        self.search.query = query;
        self.search.regex_enabled = regex_enabled;
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
        self.search.clear_results(self.pages.len());
        let query = self.search.query.to_string();
        if query.is_empty() {
            return;
        }
        let ignore_case = self.search.ignore_case;

        if self.search.regex_enabled {
            let regex = match RegexBuilder::new(&query)
                .case_insensitive(ignore_case)
                .build()
            {
                Ok(regex) => regex,
                Err(error) => {
                    self.search.regex_error = Some(error.to_string().into());
                    return;
                }
            };

            if self.text_index_state.is_pending_or_indexing() {
                return;
            }

            if self.search.search_text.is_none() {
                self.search.search_text = Some(self.searchable_document_text());
            }
            let Some(search_text) = self.search.search_text.as_ref() else {
                return;
            };
            let ranges = regex_glyph_ranges(&regex, search_text);

            for range in ranges {
                self.push_search_match(range);
            }
            return;
        }

        if self.text_index_state.is_pending_or_indexing() {
            return;
        }

        let needle = if ignore_case {
            query.to_lowercase()
        } else {
            query
        };
        for page_idx in 0..self.pages.len() {
            let search_text = self.searchable_page_text(page_idx, ignore_case);
            let mut start_byte = 0;

            while let Some(relative_start) = search_text.text[start_byte..].find(&needle) {
                let match_start = start_byte + relative_start;
                let match_end = match_start + needle.len();

                if let Some(range) = search_text.glyph_range_for_bytes(match_start..match_end) {
                    self.push_search_match(range);
                }

                start_byte = match_end;
            }
        }
    }

    fn searchable_page_text(&self, page_idx: usize, ignore_case: bool) -> SearchTextIndex {
        let mut search_text = SearchTextIndex {
            text: String::new(),
            mapping: Vec::new(),
        };
        self.append_searchable_page_text(page_idx, ignore_case, &mut search_text);
        search_text
    }

    fn searchable_document_text(&self) -> SearchTextIndex {
        let mut search_text = SearchTextIndex {
            text: String::new(),
            mapping: Vec::new(),
        };
        for page_idx in 0..self.pages.len() {
            if page_idx > 0 {
                search_text.push_separator();
            }
            self.append_searchable_page_text(page_idx, false, &mut search_text);
        }
        search_text
    }

    fn append_searchable_page_text(
        &self,
        page_idx: usize,
        ignore_case: bool,
        search_text: &mut SearchTextIndex,
    ) {
        let mut previous: Option<&Glyph> = None;
        let Some(page) = self.pages.get(page_idx) else {
            return;
        };
        let range = page.glyph_range.clone();
        if range.start >= range.end || range.end > self.glyphs.len() {
            return;
        }

        for glyph_ix in range {
            let glyph = &self.glyphs[glyph_ix];
            if let Some(previous) = previous {
                if glyph.block_idx != previous.block_idx {
                    search_text.push_separator();
                    search_text.push_separator();
                } else if glyph.line_idx != previous.line_idx || previous.inferred_space_after {
                    search_text.push_separator();
                }
            }

            if ignore_case {
                for ch in glyph.c.to_lowercase() {
                    search_text.push(ch, Some(glyph_ix));
                }
            } else {
                search_text.push(glyph.c, Some(glyph_ix));
            }
            previous = Some(glyph);
        }
    }

    fn push_search_match(&mut self, range: Range<usize>) {
        if range.start >= range.end || range.end > self.glyphs.len() {
            return;
        }
        let Some(start_page_idx) = self.glyphs.get(range.start).map(|glyph| glyph.page_idx) else {
            return;
        };
        let Some(end_page_idx) = self.glyphs.get(range.end - 1).map(|glyph| glyph.page_idx) else {
            return;
        };
        let Some(fragments) =
            search_match_page_fragments(&self.pages, start_page_idx, end_page_idx, &range)
        else {
            return;
        };

        let match_ix = self.search.matches.len();
        self.search.matches.push(SearchMatch {
            page_idx: start_page_idx,
            range: range.clone(),
        });
        for (page_idx, fragment) in fragments {
            self.search.page_matches[page_idx].push(PageSearchMatch {
                match_ix,
                range: fragment,
            });
        }
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
                self.search_match_end_doc_y(search_match)
                    .and_then(|end_y| (f64::from(end_y) >= self.scroll_y_doc).then_some(ix))
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
        self.reveal_search_match(next, cx);
        cx.notify();
    }

    pub(super) fn scroll_to_active_search_match(&mut self, cx: &mut Context<Self>) {
        if let Some(active) = self.search.active {
            self.reveal_search_match(active, cx);
        }
    }

    fn reveal_search_match(&mut self, match_ix: usize, cx: &mut Context<Self>) {
        let Some(page_idx) = self
            .search
            .matches
            .get(match_ix)
            .map(|search_match| search_match.page_idx)
        else {
            return;
        };

        self.scroll_to_search_match(match_ix, cx);
        if self.grid_overview_open {
            self.set_grid_overview_cursor(page_idx, ScrollStrategy::Center, cx);
        }
    }

    fn scroll_to_search_match(&mut self, match_ix: usize, cx: &mut Context<Self>) {
        let Some(search_match) = self.search.matches.get(match_ix) else {
            return;
        };
        let Some(doc_rect) = self.search_match_doc_rect(search_match) else {
            return;
        };

        let scale = self.effective_scale();
        let center_x = (doc_rect.x0 + doc_rect.x1) / 2.0;
        let center_y = (doc_rect.y0 + doc_rect.y1) / 2.0;

        self.set_scroll_doc(
            f64::from(center_x - self.max_page_width / 2.0),
            f64::from(center_y - self.viewport_height / (2.0 * scale)),
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

        let glyph = self.glyphs.get(search_match.range.start)?;
        if glyph.page_idx != search_match.page_idx {
            return None;
        }
        let rect = Self::quad_rect(&glyph.quad);
        let page = &self.pages[search_match.page_idx];
        let rect = Self::pdf_rect_to_view(page.bounds, self.rotation, rect);
        Some(Rect::new(
            page.offset_x + rect.x0,
            page.offset_y + rect.y0,
            page.offset_x + rect.x1,
            page.offset_y + rect.y1,
        ))
    }

    fn search_match_end_doc_y(&self, search_match: &SearchMatch) -> Option<f32> {
        if search_match.range.start >= search_match.range.end
            || search_match.range.end > self.glyphs.len()
        {
            return None;
        }

        let glyph = self.glyphs.get(search_match.range.end - 1)?;
        let page = self.pages.get(glyph.page_idx)?;
        let rect = Self::pdf_rect_to_view(page.bounds, self.rotation, Self::quad_rect(&glyph.quad));
        Some(page.offset_y + rect.y1)
    }

    pub(super) fn sync_search_bar(&mut self, cx: &mut Context<Self>) {
        if let Some(search_bar) = self.search_bar.clone() {
            let query = self.search.query.clone();
            let active = self.search.active;
            let match_count = self.search.matches.len();
            let ignore_case = self.search.ignore_case;
            let regex_enabled = self.search.regex_enabled;
            let regex_error = self.search.regex_error.clone();
            let indexing = regex_error.is_none()
                && self.text_index_state.is_pending_or_indexing()
                && !query.is_empty();
            search_bar.update(cx, |search_bar, cx| {
                search_bar.set_match_status(
                    SearchBarSnapshot {
                        query: query.clone(),
                        active_match: active,
                        match_count,
                        ignore_case,
                        regex_enabled,
                        regex_error: regex_error.clone(),
                        indexing,
                    },
                    cx,
                );
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_search_text() -> SearchTextIndex {
        SearchTextIndex {
            text: String::new(),
            mapping: Vec::new(),
        }
    }

    fn page(glyph_range: Range<usize>) -> DocumentPage {
        DocumentPage {
            bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
            width: 100.0,
            height: 100.0,
            offset_x: 0.0,
            offset_y: 0.0,
            glyph_range,
        }
    }

    #[test]
    fn regexp_matches_across_a_page_separator_as_one_glyph_range() {
        let mut search_text = empty_search_text();
        for (glyph_ix, ch) in "Alpha".chars().enumerate() {
            search_text.push(ch, Some(glyph_ix));
        }
        search_text.push_separator();
        for (offset, ch) in "beta".chars().enumerate() {
            search_text.push(ch, Some(5 + offset));
        }

        let regex = RegexBuilder::new(r"alpha\s+be[a-z]+")
            .case_insensitive(true)
            .build()
            .expect("valid regexp");

        assert_eq!(regex_glyph_ranges(&regex, &search_text), vec![0..9]);
    }

    #[test]
    fn regexp_ignores_zero_width_and_separator_only_matches() {
        let mut search_text = empty_search_text();
        search_text.push('a', Some(0));
        search_text.push_separator();
        search_text.push('b', Some(1));
        let regex = Regex::new(r"^|\n|$").expect("valid regexp");

        assert!(regex_glyph_ranges(&regex, &search_text).is_empty());
    }

    #[test]
    fn regexp_byte_offsets_map_unicode_and_non_overlapping_matches() {
        let mut search_text = empty_search_text();
        search_text.push('é', Some(0));
        search_text.push('中', Some(1));
        for glyph_ix in 2..6 {
            search_text.push('a', Some(glyph_ix));
        }

        assert_eq!(
            regex_glyph_ranges(&Regex::new("中").unwrap(), &search_text),
            vec![1..2]
        );
        assert_eq!(
            regex_glyph_ranges(&Regex::new("aa").unwrap(), &search_text),
            vec![2..4, 4..6]
        );
    }

    #[test]
    fn cross_page_match_is_split_into_ordered_page_fragments() {
        let pages = vec![page(0..3), page(3..6), page(6..6), page(6..8)];

        assert_eq!(
            search_match_page_fragments(&pages, 0, 3, &(1..7)),
            Some(vec![(0, 1..3), (1, 3..6), (3, 6..7)])
        );
    }

    #[test]
    fn invalid_regexp_is_rejected_during_compilation() {
        let invalid = String::from("[");
        assert!(RegexBuilder::new(&invalid).build().is_err());
    }

    #[test]
    fn regexp_mode_defaults_off() {
        assert!(!SearchState::default().regex_enabled);
    }
}
