use super::*;

impl DocumentView {
    pub(super) fn start_text_indexing(&mut self, cx: &mut Context<Self>) {
        if self.text_indexing_task.is_some()
            || self.pages.is_empty()
            || !self.text_index_state.begin()
        {
            return;
        }

        self.sync_search_bar(cx);

        let snapshot = self.document_snapshot.clone();
        let document_layout = self.document_layout;
        let page_count = self.pages.len();
        let page_bounds: Vec<Rect> = self.pages.iter().map(|page| page.bounds).collect();

        self.text_indexing_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;

            let result = cx
                .background_spawn(async move {
                    let chunk = Self::load_page_text_chunk(
                        &snapshot,
                        document_layout,
                        0,
                        page_count,
                        page_count,
                        page_bounds,
                    )
                    .map_err(|error| format!("{error:#}"))?;
                    Self::merge_text_chunks(vec![chunk], page_count)
                })
                .await;
            Self::finish_text_indexing(this, cx, result);
        }));
    }

    #[ztracing::instrument(skip_all, fields(start_page_idx, end_page_idx, page_count))]
    fn load_page_text_chunk(
        snapshot: &DocumentSnapshot,
        document_layout: DocumentLayout,
        start_page_idx: usize,
        end_page_idx: usize,
        page_count: usize,
        page_bounds: Vec<Rect>,
    ) -> Result<TextChunk> {
        let document = snapshot.open_with_layout(document_layout)?.into_inner();
        let mut glyphs = Vec::new();
        let mut text_lines = Vec::new();
        let mut links = Vec::new();
        let mut page_glyph_ranges = Vec::with_capacity(end_page_idx.saturating_sub(start_page_idx));
        let mut page_line_ranges = Vec::with_capacity(end_page_idx.saturating_sub(start_page_idx));
        let mut next_line_idx = 0;
        let mut next_block_idx = 0;

        for page_idx in start_page_idx..end_page_idx {
            let page = document.load_page(page_idx as i32)?;
            let page_bounds = page_bounds[page_idx - start_page_idx];
            let display_list = DisplayList::new(page_bounds)?;
            let device = Device::from_display_list(&display_list)?;
            page.run(&device, &Matrix::IDENTITY)?;

            let explicit_links_start = links.len();
            match Self::extract_explicit_links(&page, page_idx, page_count) {
                Ok(page_links) => links.extend(page_links),
                Err(error) => {
                    eprintln!(
                        "failed to read PDF links on page {}: {error:#}",
                        page_idx + 1
                    );
                }
            }

            let line_start = text_lines.len();
            let glyph_range = Self::extract_glyphs(
                &display_list,
                page_idx,
                &mut glyphs,
                &mut text_lines,
                &mut next_line_idx,
                &mut next_block_idx,
            )?;

            let detected_links = Self::detect_text_url_links(
                page_idx,
                &glyphs,
                glyph_range.clone(),
                &links[explicit_links_start..],
            );
            links.extend(detected_links);
            page_glyph_ranges.push(glyph_range);
            page_line_ranges.push(line_start..text_lines.len());
        }

        let text_blocks = Self::build_text_blocks(&glyphs);
        let mut page_block_ranges = Vec::with_capacity(page_glyph_ranges.len());
        let mut block_start = 0;
        for page_idx in start_page_idx..end_page_idx {
            let page_start = block_start;
            while text_blocks
                .get(block_start)
                .is_some_and(|block| block.page_idx == page_idx)
            {
                block_start += 1;
            }
            page_block_ranges.push(page_start..block_start);
        }

        Ok(TextChunk {
            start_page_idx,
            glyphs,
            text_blocks,
            text_lines,
            links,
            page_glyph_ranges,
            page_line_ranges,
            page_block_ranges,
            line_count: next_line_idx,
            block_count: next_block_idx,
        })
    }

    pub(super) fn merge_text_chunks(
        mut chunks: Vec<TextChunk>,
        page_count: usize,
    ) -> std::result::Result<LoadedText, String> {
        chunks.sort_by_key(|chunk| chunk.start_page_idx);

        let glyph_count = chunks.iter().map(|chunk| chunk.glyphs.len()).sum();
        let block_count = chunks.iter().map(|chunk| chunk.text_blocks.len()).sum();
        let line_count = chunks.iter().map(|chunk| chunk.text_lines.len()).sum();
        let link_count = chunks.iter().map(|chunk| chunk.links.len()).sum();
        let mut glyphs = Vec::with_capacity(glyph_count);
        let mut text_blocks = Vec::with_capacity(block_count);
        let mut text_lines = Vec::with_capacity(line_count);
        let mut links = Vec::with_capacity(link_count);
        let mut page_glyph_ranges = vec![0..0; page_count];
        let mut page_line_ranges = vec![0..0; page_count];
        let mut page_block_ranges = vec![0..0; page_count];
        let mut line_offset = 0_u32;
        let mut block_offset = 0_u32;

        for mut chunk in chunks {
            let glyph_offset = glyphs.len();

            for glyph in &mut chunk.glyphs {
                glyph.line_idx = glyph
                    .line_idx
                    .checked_add(line_offset)
                    .ok_or_else(|| "PDF contains too many text lines".to_string())?;
                glyph.block_idx = glyph
                    .block_idx
                    .checked_add(block_offset)
                    .ok_or_else(|| "PDF contains too many text blocks".to_string())?;
            }

            for block in &mut chunk.text_blocks {
                block.id = block
                    .id
                    .checked_add(block_offset)
                    .ok_or_else(|| "PDF contains too many text blocks".to_string())?;
                block.glyph_range = (block.glyph_range.start + glyph_offset)
                    ..(block.glyph_range.end + glyph_offset);
            }

            for line in &mut chunk.text_lines {
                line.glyph_range =
                    (line.glyph_range.start + glyph_offset)..(line.glyph_range.end + glyph_offset);
            }

            for (offset, range) in chunk.page_glyph_ranges.into_iter().enumerate() {
                let page_idx = chunk.start_page_idx + offset;
                if page_idx >= page_count {
                    return Err("PDF text chunk exceeded page count".to_string());
                }

                page_glyph_ranges[page_idx] =
                    (range.start + glyph_offset)..(range.end + glyph_offset);
            }

            let text_line_offset = text_lines.len();
            for (offset, range) in chunk.page_line_ranges.into_iter().enumerate() {
                let page_idx = chunk.start_page_idx + offset;
                if page_idx >= page_count {
                    return Err("PDF text chunk exceeded page count".to_string());
                }

                page_line_ranges[page_idx] =
                    (range.start + text_line_offset)..(range.end + text_line_offset);
            }

            let text_block_offset = text_blocks.len();
            for (offset, range) in chunk.page_block_ranges.into_iter().enumerate() {
                let page_idx = chunk.start_page_idx + offset;
                if page_idx >= page_count {
                    return Err("PDF text chunk exceeded page count".to_string());
                }

                page_block_ranges[page_idx] =
                    (range.start + text_block_offset)..(range.end + text_block_offset);
            }

            line_offset = line_offset
                .checked_add(chunk.line_count)
                .ok_or_else(|| "PDF contains too many text lines".to_string())?;
            block_offset = block_offset
                .checked_add(chunk.block_count)
                .ok_or_else(|| "PDF contains too many text blocks".to_string())?;

            glyphs.extend(chunk.glyphs);
            text_blocks.extend(chunk.text_blocks);
            text_lines.extend(chunk.text_lines);
            links.extend(chunk.links);
        }

        Ok(LoadedText {
            glyphs,
            text_blocks,
            text_lines,
            links,
            page_glyph_ranges,
            page_line_ranges,
            page_block_ranges,
        })
    }

    fn finish_text_indexing(
        this: gpui::WeakEntity<Self>,
        cx: &mut AsyncApp,
        result: std::result::Result<LoadedText, String>,
    ) {
        let update_result = this.update(cx, |this, cx| {
            this.text_indexing_task = None;
            match result {
                Ok(loaded) => this.apply_text_data(loaded, cx),
                Err(error) => this.apply_text_indexing_error(error, cx),
            }
        });
        if let Err(error) = update_result
            && error.to_string() != "entity released"
        {
            eprintln!("failed to update indexed PDF text: {error:#}");
        }
    }

    fn apply_text_data(&mut self, loaded: LoadedText, cx: &mut Context<Self>) {
        self.glyphs = loaded.glyphs;
        self.text_blocks = loaded.text_blocks;
        self.text_lines = loaded.text_lines;
        self.page_line_ranges = loaded.page_line_ranges;
        self.page_block_ranges = loaded.page_block_ranges;
        self.invalidate_translation_document();
        self.links = loaded.links;
        self.text_index_state.settle(true);
        self.hovered_link = None;
        self.pending_link = None;
        self.pending_selection_anchor = None;
        for source in self.page_render_sources.values_mut() {
            source.text_revision = None;
        }

        for (page, glyph_range) in self.pages.iter_mut().zip(loaded.page_glyph_ranges) {
            page.glyph_range = glyph_range;
        }

        self.search.invalidate_search_text();
        self.refresh_annotation_geometry();
        self.refresh_notes_sidebar_entries();
        if let Some(position) = self.pending_semantic_position.take() {
            self.restore_position(position);
        }
        if self.capabilities.reflow {
            self.record_current_history_position(cx);
            Self::persist_history_entry(&self.path, cx);
        }
        self.recompute_search();
        self.search.active = self.nearest_search_match_index();
        let reveal_search_match = self.search.take_reveal_after_indexing();
        self.sync_search_bar(cx);

        if reveal_search_match && self.search.active.is_some() {
            self.record_search_jump_origin();
            self.scroll_to_active_search_match(cx);
        }

        if self.translation.view_language == DocumentViewLanguage::Translated
            && self.translation.session_id.is_some()
        {
            self.refresh_translation_session(cx);
        }
        cx.notify();
    }

    fn invalidate_translation_document(&mut self) {
        self.translation.hovered_region = None;
        self.translation.external_hovered_region = None;
        self.translation.hover_animation = None;
        self.translation.hover_clear_generation =
            self.translation.hover_clear_generation.wrapping_add(1);
        self.translation.hover_clear_task = None;
        self.translation.hover_tools_hovered = false;
        self.translation.hover_tools_bounds = None;
    }

    pub(super) fn replace_translation_document(&mut self) {
        self.invalidate_translation_document();
        self.translation.request_generation = self.translation.request_generation.wrapping_add(1);
        self.reset_translation_scroll(false);
        self.reset_translation_scroll(true);
        reset_translation_cards_for_document_replacement(
            &mut self.translation.popup,
            &mut self.translation.pinned,
            self.translation.request_generation,
        );
    }

    #[ztracing::instrument(skip_all, fields(glyph_count = glyphs.len()))]
    pub(super) fn build_text_blocks(glyphs: &[Glyph]) -> Vec<TextBlock> {
        let mut blocks = Vec::new();
        let mut start = 0;
        while start < glyphs.len() {
            let first = &glyphs[start];
            let block_id = first.block_idx;
            let mut end = start + 1;
            while end < glyphs.len() && glyphs[end].block_idx == block_id {
                end += 1;
            }
            blocks.push(TextBlock {
                id: block_id,
                page_idx: first.page_idx,
                glyph_range: start..end,
            });
            start = end;
        }
        blocks
    }

    fn apply_text_indexing_error(&mut self, error: String, cx: &mut Context<Self>) {
        eprintln!("failed to index PDF text: {error}");
        self.text_index_state.settle(false);
        self.search.cancel_reveal_after_indexing();
        self.search.invalidate_search_text();
        self.recompute_search();
        self.sync_search_bar(cx);
        cx.notify();
    }
}
