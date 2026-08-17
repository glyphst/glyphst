use super::*;

impl DocumentView {
    pub(super) fn sync_inherited_target_language(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if cx
            .global::<LibraryStore>()
            .document_target_language(&self.path)
            .is_some()
        {
            return;
        }
        let target = cx
            .global::<SettingsStore>()
            .settings
            .translation
            .resolved_target_language();
        let Ok(target) = LanguageCode::new(target) else {
            return;
        };
        if self.target_language == target.as_str() {
            return;
        }
        self.target_language = target.as_str().to_string();
        for select in [
            self.translation_popup_language_select.as_ref(),
            self.pinned_translation_language_select.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            let value = self.target_language.clone();
            select.state.update(cx, |select, cx| {
                select.set_selected_value(&value, window, cx);
            });
        }
        cx.notify();
    }
}

fn translation_hover_region_id(page_index: usize, namespace: &str, source_id: &str) -> String {
    format!("page:{page_index}:{namespace}:{source_id}")
}

pub(super) fn replace_page_text_data(
    pages: &mut [DocumentPage],
    glyphs: &mut Vec<Glyph>,
    text_lines: &mut Vec<TextLine>,
    text_blocks: &mut Vec<TextBlock>,
    page_line_ranges: &mut [Range<usize>],
    page_block_ranges: &mut [Range<usize>],
    page_index: usize,
    mut replacement: PreparedPageText,
) -> std::result::Result<(), String> {
    if page_line_ranges.len() != pages.len() || page_block_ranges.len() != pages.len() {
        return Err("PDF page text range counts were inconsistent".to_string());
    }
    if text_lines.iter().any(|line| {
        line.glyph_range.start > line.glyph_range.end || line.glyph_range.end > glyphs.len()
    }) || text_blocks.iter().any(|block| {
        block.glyph_range.start > block.glyph_range.end || block.glyph_range.end > glyphs.len()
    }) || pages.iter().any(|page| {
        page.glyph_range.start > page.glyph_range.end || page.glyph_range.end > glyphs.len()
    }) || page_line_ranges
        .iter()
        .any(|range| range.start > range.end || range.end > text_lines.len())
        || page_block_ranges
            .iter()
            .any(|range| range.start > range.end || range.end > text_blocks.len())
    {
        return Err("Existing PDF text ranges were inconsistent".to_string());
    }
    let page = pages
        .get(page_index)
        .ok_or_else(|| "Translated preview page exceeded the document page count".to_string())?;
    let old_glyph_range = page.glyph_range.clone();
    let old_line_range = page_line_ranges
        .get(page_index)
        .cloned()
        .ok_or_else(|| "Translated preview page had no text-line range".to_string())?;
    let old_block_range = page_block_ranges
        .get(page_index)
        .cloned()
        .ok_or_else(|| "Translated preview page had no text-block range".to_string())?;
    if old_glyph_range.end > glyphs.len()
        || old_line_range.end > text_lines.len()
        || old_block_range.end > text_blocks.len()
        || old_glyph_range.start > old_glyph_range.end
        || old_line_range.start > old_line_range.end
        || old_block_range.start > old_block_range.end
    {
        return Err("Existing PDF text ranges were inconsistent".to_string());
    }
    let total_line_count = (text_lines.len() - old_line_range.len())
        .checked_add(replacement.text_lines.len())
        .ok_or_else(|| "PDF contains too many text lines".to_string())?;
    let total_block_count = (text_blocks.len() - old_block_range.len())
        .checked_add(replacement.text_blocks.len())
        .ok_or_else(|| "PDF contains too many text blocks".to_string())?;
    if total_line_count > u32::MAX as usize {
        return Err("PDF contains too many text lines".to_string());
    }
    if total_block_count > u32::MAX as usize {
        return Err("PDF contains too many text blocks".to_string());
    }
    if replacement.text_lines.iter().any(|line| {
        line.glyph_range.start > line.glyph_range.end
            || line.glyph_range.end > replacement.glyphs.len()
    }) || replacement.text_blocks.iter().any(|block| {
        block.glyph_range.start > block.glyph_range.end
            || block.glyph_range.end > replacement.glyphs.len()
    }) {
        return Err("Translated PDF text ranges were inconsistent".to_string());
    }

    let new_glyph_end = old_glyph_range
        .start
        .checked_add(replacement.glyphs.len())
        .ok_or_else(|| "PDF contains too many glyphs".to_string())?;
    let new_line_end = old_line_range
        .start
        .checked_add(replacement.text_lines.len())
        .ok_or_else(|| "PDF contains too many text lines".to_string())?;
    let new_block_end = old_block_range
        .start
        .checked_add(replacement.text_blocks.len())
        .ok_or_else(|| "PDF contains too many text blocks".to_string())?;
    let remap_after = |index: usize, old_end: usize, new_end: usize| {
        index
            .checked_sub(old_end)
            .and_then(|offset| new_end.checked_add(offset))
            .ok_or_else(|| "PDF page text ranges were out of order".to_string())
    };

    let shifted_later_lines = text_lines[old_line_range.end..]
        .iter()
        .map(|line| {
            Ok(
                remap_after(line.glyph_range.start, old_glyph_range.end, new_glyph_end)?
                    ..remap_after(line.glyph_range.end, old_glyph_range.end, new_glyph_end)?,
            )
        })
        .collect::<std::result::Result<Vec<_>, String>>()?;
    let shifted_later_blocks = text_blocks[old_block_range.end..]
        .iter()
        .map(|block| {
            Ok(
                remap_after(block.glyph_range.start, old_glyph_range.end, new_glyph_end)?
                    ..remap_after(block.glyph_range.end, old_glyph_range.end, new_glyph_end)?,
            )
        })
        .collect::<std::result::Result<Vec<_>, String>>()?;
    let shifted_later_pages = pages[page_index + 1..]
        .iter()
        .map(|page| {
            Ok(
                remap_after(page.glyph_range.start, old_glyph_range.end, new_glyph_end)?
                    ..remap_after(page.glyph_range.end, old_glyph_range.end, new_glyph_end)?,
            )
        })
        .collect::<std::result::Result<Vec<_>, String>>()?;
    let shifted_later_page_lines = page_line_ranges[page_index + 1..]
        .iter()
        .map(|range| {
            Ok(remap_after(range.start, old_line_range.end, new_line_end)?
                ..remap_after(range.end, old_line_range.end, new_line_end)?)
        })
        .collect::<std::result::Result<Vec<_>, String>>()?;
    let shifted_later_page_blocks = page_block_ranges[page_index + 1..]
        .iter()
        .map(|range| {
            Ok(
                remap_after(range.start, old_block_range.end, new_block_end)?
                    ..remap_after(range.end, old_block_range.end, new_block_end)?,
            )
        })
        .collect::<std::result::Result<Vec<_>, String>>()?;

    for line in &mut replacement.text_lines {
        line.glyph_range = (line.glyph_range.start + old_glyph_range.start)
            ..(line.glyph_range.end + old_glyph_range.start);
    }
    for block in &mut replacement.text_blocks {
        block.glyph_range = (block.glyph_range.start + old_glyph_range.start)
            ..(block.glyph_range.end + old_glyph_range.start);
    }
    for (line, range) in text_lines[old_line_range.end..]
        .iter_mut()
        .zip(shifted_later_lines)
    {
        line.glyph_range = range;
    }
    for (block, range) in text_blocks[old_block_range.end..]
        .iter_mut()
        .zip(shifted_later_blocks)
    {
        block.glyph_range = range;
    }
    for (later_page, range) in pages[page_index + 1..].iter_mut().zip(shifted_later_pages) {
        later_page.glyph_range = range;
    }
    for (range, shifted) in page_line_ranges[page_index + 1..]
        .iter_mut()
        .zip(shifted_later_page_lines)
    {
        *range = shifted;
    }
    for (range, shifted) in page_block_ranges[page_index + 1..]
        .iter_mut()
        .zip(shifted_later_page_blocks)
    {
        *range = shifted;
    }

    glyphs.splice(old_glyph_range.clone(), replacement.glyphs);
    text_lines.splice(old_line_range.clone(), replacement.text_lines);
    text_blocks.splice(old_block_range.clone(), replacement.text_blocks);
    pages[page_index].glyph_range = old_glyph_range.start..new_glyph_end;
    page_line_ranges[page_index] = old_line_range.start..new_line_end;
    page_block_ranges[page_index] = old_block_range.start..new_block_end;

    for (line_index, line) in text_lines.iter().enumerate() {
        let line_index = line_index as u32;
        for glyph in &mut glyphs[line.glyph_range.clone()] {
            glyph.line_idx = line_index;
        }
    }
    for (block_index, block) in text_blocks.iter_mut().enumerate() {
        let block_index = block_index as u32;
        block.id = block_index;
        for glyph in &mut glyphs[block.glyph_range.clone()] {
            glyph.block_idx = block_index;
        }
    }
    Ok(())
}

impl DocumentView {
    pub(super) fn can_translate_document(&self) -> bool {
        self.format == DocumentFormat::Pdf && self.path.is_absolute()
    }

    #[cfg(test)]
    pub(super) fn document_translation_output_path(
        source: &Path,
        target: &LanguageCode,
    ) -> PathBuf {
        let stem = source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .unwrap_or("document");
        source
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(format!("{stem}.{}.pdf", target.as_str()))
    }

    pub(super) fn translate_document(&mut self, cx: &mut Context<Self>) {
        if !self.can_translate_document() {
            return;
        }
        let source_path = self.path.clone();
        if self
            .document_translation_jobs
            .read(cx)
            .has_active_session_for_source(&source_path)
        {
            return;
        }

        if !cx.has_global::<PdfTranslationService>() {
            self.document_translation_jobs.update(cx, |jobs, cx| {
                jobs.report_preflight_error(
                    source_path,
                    DocumentTranslationOpenMode::Document,
                    "Document translation is unavailable.",
                    cx,
                );
            });
            return;
        }

        let (settings, credentials_loading) = {
            let store = cx.global::<SettingsStore>();
            (
                store.settings.clone(),
                store.document_translation_credentials_loading(),
            )
        };
        if credentials_loading {
            self.document_translation_jobs.update(cx, |jobs, cx| {
                jobs.report_preflight_error(
                    source_path,
                    DocumentTranslationOpenMode::Document,
                    "Provider credentials are still loading. Try again in a moment.",
                    cx,
                );
            });
            return;
        }
        let source_language = settings
            .translation
            .source_language
            .as_deref()
            .and_then(|language| LanguageCode::new(language).ok());
        let target_language =
            match LanguageCode::new(settings.translation.resolved_target_language()) {
                Ok(language) => language,
                Err(_) => {
                    let source_path = self.path.clone();
                    self.document_translation_jobs.update(cx, |jobs, cx| {
                        jobs.report_preflight_error(
                            source_path,
                            DocumentTranslationOpenMode::Document,
                            "Choose a valid target language in Translation Settings.",
                            cx,
                        );
                    });
                    return;
                }
            };
        let mode = settings.translation.document_mode;
        let source_snapshot = self.document_snapshot.clone();
        let request = PdfTranslationSessionRequest {
            source_path,
            source_language,
            target_language,
        };
        self.document_translation_jobs.update(cx, |jobs, cx| {
            jobs.submit_session(settings, request, source_snapshot, mode, cx)
        });
    }

    pub(super) fn cancel_document_translation(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.translation_session_id {
            self.document_translation_jobs
                .update(cx, |jobs, cx| jobs.cancel_session(id, cx));
        } else {
            let source_path = self.path.clone();
            self.document_translation_jobs.update(cx, |jobs, cx| {
                jobs.cancel_for_source(&source_path, cx);
            });
        }
    }

    /// Turn this document view into the live translated side of a session.
    /// Individual pages are replaced by revisioned, in-memory PDFs produced by
    /// the canonical layout/Typst pipeline.
    pub fn attach_translation_session(
        &mut self,
        id: DocumentTranslationJobId,
        cx: &mut Context<Self>,
    ) {
        if self.translation_session_id.is_none() {
            self.file_name = format!("{} — Translation", self.file_name).into();
        }
        self.translation_session_id = Some(id);
        self.translation_pair_session_id = Some(id);
        self.translation_view_role = TranslationViewRole::Translation;
        self.translation_pair_active = false;
        self.translation_overlay_enabled = true;
        self.translation_position_sync_ready = false;
        self.pending_translation_viewport_anchor = None;
        self.view_language = DocumentViewLanguage::Translated;
        self.cancel_translation_panel_pending_dock();
        self.cancel_translation_panel_retract();
        self.cancel_translation_panel_initial_dock();
        self.translation_panel_placement = TranslationPanelPlacement::Default;
        self.translation_panel_drag = None;
        self.translation_panel_initial_dock_armed = true;
        self.translation_panel_dock_panel_hovered = false;
        self.translation_panel_dock_edge_hovered = false;
        self.refresh_translation_session(cx);
        self.sync_translation_panel_initial_dock(cx);
        cx.notify();
    }

    /// Attach the original view as the passive half of a live translation
    /// pair. This view observes semantic mappings but never swaps its pages or
    /// initiates hover events.
    pub fn attach_translation_source_mirror(
        &mut self,
        id: DocumentTranslationJobId,
        cx: &mut Context<Self>,
    ) {
        self.translation_pair_session_id = Some(id);
        self.translation_view_role = TranslationViewRole::SourceMirror;
        self.translation_pair_active = false;
        self.translation_overlay_enabled = true;
        self.translation_position_sync_ready = true;
        self.pending_translation_viewport_anchor = None;
        self.view_language = DocumentViewLanguage::Original;
        self.refresh_translation_session(cx);
        cx.notify();
    }

    /// Enable paired translation affordances only while both tabs are the
    /// selected tabs in their panes. The session data remains attached while
    /// inactive so returning to the pair is instantaneous.
    pub fn set_translation_pair_active(
        &mut self,
        id: DocumentTranslationJobId,
        active: bool,
        cx: &mut Context<Self>,
    ) {
        if self.translation_pair_session_id != Some(id)
            || self.translation_view_role == TranslationViewRole::None
            || self.translation_pair_active == active
        {
            return;
        }
        self.translation_pair_active = active;
        if !active {
            self.clear_translation_hover_state(true, cx);
            self.cancel_translation_panel_pending_dock();
            self.cancel_translation_panel_retract();
            self.cancel_translation_panel_initial_dock();
            self.translation_panel_drag = None;
            self.translation_panel_dock_panel_hovered = false;
            self.translation_panel_dock_edge_hovered = false;
        } else {
            self.sync_translation_panel_initial_dock(cx);
        }
        cx.notify();
    }

    /// Restore an original document to its ordinary interaction role after
    /// the paired live-translation tab is closed or backgrounded.
    pub fn detach_translation_source_mirror(
        &mut self,
        id: DocumentTranslationJobId,
        cx: &mut Context<Self>,
    ) {
        if self.translation_view_role != TranslationViewRole::SourceMirror
            || self.translation_pair_session_id != Some(id)
        {
            return;
        }
        self.translation_pair_session_id = None;
        self.translation_view_role = TranslationViewRole::None;
        self.translation_pair_active = false;
        self.translation_overlay_enabled = true;
        self.translation_position_sync_ready = false;
        self.pending_translation_viewport_anchor = None;
        self.translation_session_snapshot = None;
        self.translation_hover_regions.clear();
        self.translation_failed_hover_regions.clear();
        self.clear_translation_hover_state(true, cx);
        self.cancel_translation_panel_pending_dock();
        self.cancel_translation_panel_retract();
        self.cancel_translation_panel_initial_dock();
        self.translation_panel_initial_dock_armed = false;
        self.translation_panel_drag = None;
        self.translation_panel_dock_panel_hovered = false;
        self.translation_panel_dock_edge_hovered = false;
        cx.notify();
    }

    pub fn translation_session_id(&self) -> Option<DocumentTranslationJobId> {
        self.translation_session_id
    }

    pub fn translation_overlay_enabled(&self) -> bool {
        self.translation_overlay_enabled
    }

    pub fn translation_session_is_running(&self) -> bool {
        self.translation_session_snapshot
            .as_ref()
            .is_some_and(DocumentTranslationSessionSnapshot::is_translating)
    }

    pub(super) fn refresh_translation_session(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self
            .translation_session_id
            .or(self.translation_pair_session_id)
        else {
            return;
        };
        let Some(snapshot) = self.document_translation_jobs.read(cx).session_snapshot(id) else {
            self.translation_session_snapshot = None;
            self.translation_hover_regions.clear();
            self.translation_failed_hover_regions.clear();
            self.translation_pair_active = false;
            self.clear_translation_hover_state(true, cx);
            self.cancel_translation_panel_pending_dock();
            self.cancel_translation_panel_retract();
            self.cancel_translation_panel_initial_dock();
            self.translation_panel_initial_dock_armed = false;
            self.translation_panel_drag = None;
            self.translation_panel_dock_panel_hovered = false;
            self.translation_panel_dock_edge_hovered = false;
            if self.translation_view_role == TranslationViewRole::Translation {
                self.clear_translation_page_sources(cx);
            }
            cx.notify();
            return;
        };
        self.target_language = snapshot.request.target_language.as_str().to_string();
        self.translation_session_snapshot = Some(snapshot);
        if self.translation_view_role == TranslationViewRole::Translation {
            self.refresh_translation_page_sources(cx);
        }
        self.rebuild_session_hover_regions();
        self.sync_translation_panel_initial_dock(cx);
        cx.notify();
    }

    fn clear_translation_page_sources(&mut self, cx: &mut Context<Self>) {
        if self.page_render_sources.is_empty() {
            return;
        }
        let pages = self.page_render_sources.keys().copied().collect::<Vec<_>>();
        self.page_render_sources.clear();
        self.pending_renders
            .retain(|pending| !pages.contains(&pending.key.page_idx));
        for page_index in pages {
            let removed = cx
                .global_mut::<RenderCache>()
                .evict_page(self.doc_id, page_index);
            for image in removed {
                cx.drop_image(image, None);
            }
            if let Some(bounds) = self.page_image_bounds.get_mut(page_index) {
                *bounds = None;
            }
        }
        self.restore_original_page_text(cx);
    }

    fn refresh_translation_page_sources(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.translation_session_snapshot.as_ref() else {
            return;
        };
        let previews = snapshot.page_previews.clone();
        if previews.is_empty() && !self.page_render_sources.is_empty() {
            self.clear_translation_page_sources(cx);
        }
        let mut text_changed = false;
        for (page_index, preview) in previews.iter() {
            let current = self.page_render_sources.get(page_index);
            if current.is_some_and(|source| source.revision > preview.revision) {
                continue;
            }
            let replace_render = current.is_none_or(|source| source.revision < preview.revision);
            let replace_text = replace_render
                || current.is_none_or(|source| source.text_revision != Some(preview.revision));
            if !replace_render && !replace_text {
                continue;
            }
            let prepared_text =
                match Self::prepare_translation_page_text(*page_index, &preview.text_layer) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        eprintln!(
                            "failed to prepare translated text geometry for page {}: {error}",
                            page_index + 1
                        );
                        continue;
                    }
                };
            let replacement_source = if replace_render {
                let path = PathBuf::from(format!(
                    "translation-page-{}-revision-{}.pdf",
                    page_index + 1,
                    preview.revision
                ));
                let document_snapshot =
                    Arc::new(DocumentSnapshot::from_pdf_bytes(path, preview.pdf.clone()));
                let render_session = match DocumentRenderSession::open(
                    document_snapshot.clone(),
                    DocumentLayout::Fixed,
                ) {
                    Ok(render_session) => render_session,
                    Err(error) => {
                        eprintln!(
                            "failed to open translated preview for page {}: {error:#}",
                            page_index + 1
                        );
                        continue;
                    }
                };
                Some(RevisionedPageRenderSource {
                    revision: preview.revision,
                    text_revision: Some(preview.revision),
                    _snapshot: document_snapshot,
                    render_session,
                })
            } else {
                None
            };
            if let Err(error) = self.replace_translation_page_text(*page_index, prepared_text) {
                eprintln!(
                    "failed to install translated text geometry for page {}: {error}",
                    page_index + 1
                );
                continue;
            }
            text_changed = true;
            if let Some(replacement_source) = replacement_source {
                self.page_render_sources
                    .insert(*page_index, replacement_source);
                self.pending_renders
                    .retain(|pending| pending.key.page_idx != *page_index);
                let removed = cx
                    .global_mut::<RenderCache>()
                    .evict_page(self.doc_id, *page_index);
                for image in removed {
                    cx.drop_image(image, None);
                }
                if let Some(bounds) = self.page_image_bounds.get_mut(*page_index) {
                    *bounds = None;
                }
            } else if let Some(source) = self.page_render_sources.get_mut(page_index) {
                source.text_revision = Some(preview.revision);
            }
        }
        if text_changed {
            self.finish_translation_page_text_update(cx);
        }
    }

    fn prepare_translation_page_text(
        page_index: usize,
        layer: &PdfTranslationPageTextLayer,
    ) -> std::result::Result<PreparedPageText, String> {
        let mut glyphs = Vec::new();
        let mut text_lines = Vec::new();
        let mut text_blocks = Vec::new();
        for block in &layer.blocks {
            let block_index = u32::try_from(text_blocks.len())
                .map_err(|_| "PDF page contains too many text blocks".to_string())?;
            let block_start = glyphs.len();
            for line in &block.lines {
                let line_index = u32::try_from(text_lines.len())
                    .map_err(|_| "PDF page contains too many text lines".to_string())?;
                let line_start = glyphs.len();
                let line_rect = Rect::new(
                    line.bounds.x0,
                    line.bounds.y0,
                    line.bounds.x1,
                    line.bounds.y1,
                );
                glyphs.extend(line.glyphs.iter().map(|glyph| Glyph {
                    c: glyph.character,
                    quad: Quad::new(
                        mupdf::Point::new(glyph.quad.upper_left.x, glyph.quad.upper_left.y),
                        mupdf::Point::new(glyph.quad.upper_right.x, glyph.quad.upper_right.y),
                        mupdf::Point::new(glyph.quad.lower_left.x, glyph.quad.lower_left.y),
                        mupdf::Point::new(glyph.quad.lower_right.x, glyph.quad.lower_right.y),
                    ),
                    line_rect,
                    page_idx: page_index,
                    line_idx: line_index,
                    block_idx: block_index,
                    font_size: glyph.font_size,
                    is_bold: glyph.bold,
                    inferred_space_after: false,
                }));
                if line_start < glyphs.len() {
                    Self::mark_inferred_spaces(&mut glyphs[line_start..]);
                    text_lines.push(TextLine {
                        glyph_range: line_start..glyphs.len(),
                        bounds: line_rect,
                    });
                }
            }
            if block_start < glyphs.len() {
                text_blocks.push(TextBlock {
                    id: block_index,
                    page_idx: page_index,
                    glyph_range: block_start..glyphs.len(),
                });
            }
        }
        Ok(PreparedPageText {
            glyphs,
            text_blocks,
            text_lines,
        })
    }

    fn replace_translation_page_text(
        &mut self,
        page_index: usize,
        prepared: PreparedPageText,
    ) -> std::result::Result<(), String> {
        replace_page_text_data(
            &mut self.pages,
            &mut self.glyphs,
            &mut self.text_lines,
            &mut self.text_blocks,
            &mut self.page_line_ranges,
            &mut self.page_block_ranges,
            page_index,
            prepared,
        )
    }

    fn finish_translation_page_text_update(&mut self, cx: &mut Context<Self>) {
        self.selection = None;
        self.pending_selection_anchor = None;
        self.dragging = false;
        self.hovered_link = None;
        self.pending_link = None;
        self.refresh_annotation_geometry();
        self.refresh_notes_sidebar_entries();
        self.recompute_search();
        self.search.active = self.nearest_search_match_index();
        self.sync_search_bar(cx);
        cx.notify();
    }

    fn restore_original_page_text(&mut self, cx: &mut Context<Self>) {
        self.text_indexing_task = None;
        self.glyphs.clear();
        self.text_lines.clear();
        self.text_blocks.clear();
        self.links.clear();
        self.page_line_ranges = vec![0..0; self.pages.len()];
        self.page_block_ranges = vec![0..0; self.pages.len()];
        for page in &mut self.pages {
            page.glyph_range = 0..0;
        }
        self.selection = None;
        self.pending_selection_anchor = None;
        self.hovered_link = None;
        self.pending_link = None;
        self.text_index_state = TextIndexState::Pending;
        self.recompute_search();
        self.sync_search_bar(cx);
        self.start_text_indexing(cx);
    }

    fn rebuild_session_hover_regions(&mut self) {
        let Some(snapshot) = self.translation_session_snapshot.as_ref() else {
            return;
        };
        let unit_ids = snapshot
            .resolved_units
            .iter()
            .flat_map(|unit| {
                unit.member_block_ids
                    .iter()
                    .map(move |block_id| (block_id.as_str(), unit.id.as_str()))
            })
            .collect::<HashMap<_, _>>();
        let mut regions = HashMap::<String, TranslationHoverRegionEntry>::new();
        for preview in snapshot.page_previews.values() {
            for region in &preview.hover_regions {
                regions.insert(
                    region.id.clone(),
                    TranslationHoverRegionEntry {
                        page_index: preview.page_index,
                        unit_id: region.unit_id.clone(),
                        block_id: region.block_id.clone(),
                        target_bounds: region.target_bounds,
                        source_regions: region.source_regions.clone(),
                    },
                );
            }
        }

        // Prepared pages provide stable fallback geometry before a preview is
        // ready and for content deliberately left untouched by translation.
        if let Some(document) = snapshot.prepared_document.as_ref() {
            for page in &document.pages {
                for block in &page.blocks {
                    if block.kind != BlockKind::Text
                        && page.protected_regions.iter().any(|protected| {
                            protected
                                .bbox
                                .intersection(block.bbox)
                                .is_some_and(|intersection| {
                                    intersection.area()
                                        / protected.bbox.area().min(block.bbox.area()).max(1.0)
                                        >= 0.8
                                })
                        })
                    {
                        continue;
                    }
                    let region_id =
                        translation_hover_region_id(page.page_index, "block", block.id.as_str());
                    regions
                        .entry(region_id)
                        .or_insert_with(|| TranslationHoverRegionEntry {
                            page_index: page.page_index,
                            unit_id: unit_ids
                                .get(block.id.as_str())
                                .map(|unit_id| (*unit_id).to_string()),
                            block_id: Some(block.id.clone()),
                            target_bounds: block.bbox,
                            source_regions: source_text_regions(block),
                        });
                }
                for protected in &page.protected_regions {
                    let region_id = translation_hover_region_id(
                        page.page_index,
                        "protected",
                        protected.id.as_str(),
                    );
                    regions
                        .entry(region_id)
                        .or_insert_with(|| TranslationHoverRegionEntry {
                            page_index: page.page_index,
                            unit_id: None,
                            block_id: None,
                            target_bounds: protected.bbox,
                            source_regions: vec![protected.bbox],
                        });
                }
            }
        }

        let failed_block_ids = snapshot
            .failures
            .iter()
            .flat_map(|failure| failure.member_block_ids.iter().cloned())
            .collect::<HashSet<_>>();
        let mut failed_regions = HashSet::new();
        for (region_id, region) in &regions {
            if region
                .block_id
                .as_ref()
                .is_some_and(|block_id| failed_block_ids.contains(block_id))
            {
                failed_regions.insert(region_id.clone());
            }
        }
        self.translation_hover_regions = regions;
        self.translation_failed_hover_regions = failed_regions;
    }

    pub(super) fn toggle_translation_unit(&mut self, unit_id: &str, cx: &mut Context<Self>) {
        let Some(id) = self.translation_session_id else {
            return;
        };
        self.document_translation_jobs.update(cx, |manager, cx| {
            manager.toggle_session_unit_original(id, unit_id, cx);
        });
    }

    pub(super) fn retry_failed_translation_units(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.translation_session_id {
            self.document_translation_jobs.update(cx, |manager, cx| {
                manager.retry_session_failures(id, cx);
            });
        }
    }

    pub(super) fn resume_translation_session(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.translation_session_id {
            self.document_translation_jobs.update(cx, |manager, cx| {
                manager.resume_session(id, cx);
            });
        }
    }

    pub(super) fn request_save_translation_session(&self, cx: &mut Context<Self>) {
        if let Some(id) = self.translation_session_id {
            self.document_translation_jobs.update(cx, |manager, cx| {
                manager.request_save_session(id, cx);
            });
        }
    }

    pub(super) fn translate_selection(
        &mut self,
        popup_position: Option<GpuiPoint<Pixels>>,
        update_pinned: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(text) = self.selected_source_text() else {
            return;
        };
        self.translate_text(text, popup_position, update_pinned, cx);
    }

    pub(super) fn reset_translation_scroll(&self, pinned: bool) {
        let scroll_handle = if pinned {
            &self.pinned_translation_scroll
        } else {
            &self.translation_popup_scroll
        };
        scroll_handle.set_offset(point(px(0.0), px(0.0)));
    }

    fn translate_text(
        &mut self,
        text: String,
        popup_position: Option<GpuiPoint<Pixels>>,
        update_pinned: bool,
        cx: &mut Context<Self>,
    ) {
        let (settings, credentials_loading) = {
            let store = cx.global::<SettingsStore>();
            (
                store.settings.clone(),
                store.translation_credentials_loading(),
            )
        };
        if credentials_loading {
            self.set_translation_error(
                popup_position,
                update_pinned,
                text,
                "Provider credentials are still loading. Try again in a moment.".to_string(),
                cx,
            );
            return;
        }
        let target_language = match LanguageCode::new(&self.target_language) {
            Ok(language) => language,
            Err(error) => {
                self.set_translation_error(
                    popup_position,
                    update_pinned,
                    text,
                    error.to_string(),
                    cx,
                );
                return;
            }
        };
        self.translation_request_generation = self.translation_request_generation.wrapping_add(1);
        let generation = self.translation_request_generation;
        let state = TranslationCardState::new(
            text.clone(),
            TranslationStatus::Loading,
            popup_position,
            generation,
        );
        if popup_position.is_some() {
            self.reset_translation_scroll(false);
            self.translation_popup = Some(state);
        }
        if update_pinned {
            self.reset_translation_scroll(true);
            self.pinned_translation = Some(TranslationCardState::new(
                text.clone(),
                TranslationStatus::Loading,
                None,
                generation,
            ));
        }
        cx.notify();

        if !cx.has_global::<TranslationService>() {
            self.finish_translation_request(
                generation,
                Err("translation service is unavailable".to_string()),
                cx,
            );
            return;
        }

        let service = cx.global::<TranslationService>().clone();
        let provider = settings.translation.active_provider;
        let source_language = settings
            .translation
            .source_language
            .as_deref()
            .and_then(|language| LanguageCode::new(language).ok());
        let request = TranslationRequest {
            provider,
            source_language,
            target_language,
            texts: vec![text],
            context_before: None,
            context_after: None,
        };

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    service
                        .translate(&settings, request)
                        .map(|response| {
                            response.translations.into_iter().next().unwrap_or_default()
                        })
                        .map_err(|error| error.to_string())
                })
                .await;
            _ = this.update(cx, |this, cx| {
                this.finish_translation_request(generation, result, cx);
            });
        })
        .detach();
    }

    fn set_translation_error(
        &mut self,
        popup_position: Option<GpuiPoint<Pixels>>,
        update_pinned: bool,
        source: String,
        error: String,
        cx: &mut Context<Self>,
    ) {
        self.translation_request_generation = self.translation_request_generation.wrapping_add(1);
        let error: SharedString = error.into();
        let state = TranslationCardState::new(
            source.clone(),
            TranslationStatus::Error(error.clone()),
            popup_position,
            self.translation_request_generation,
        );
        if popup_position.is_some() {
            self.reset_translation_scroll(false);
            self.translation_popup = Some(state);
        }
        if update_pinned {
            self.reset_translation_scroll(true);
            self.pinned_translation = Some(TranslationCardState::new(
                source,
                TranslationStatus::Error(error),
                None,
                self.translation_request_generation,
            ));
        }
        cx.notify();
    }

    fn finish_translation_request(
        &mut self,
        generation: u64,
        result: std::result::Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        let status = match result {
            Ok(text) if !text.trim().is_empty() => TranslationStatus::Ready {
                markdown: cx.new(|cx| Markdown::new(text.clone().into(), cx)),
                text: text.into(),
            },
            Ok(_) => TranslationStatus::Error(crate::fl!("translation-no-result").into()),
            Err(error) => TranslationStatus::Error(error.into()),
        };
        if let Some(popup) = self.translation_popup.as_mut()
            && popup.request_generation == generation
        {
            popup.status = status.clone();
        }
        if let Some(pinned) = self.pinned_translation.as_mut()
            && pinned.request_generation == generation
        {
            pinned.status = status;
        }
        cx.notify();
    }

    pub(super) fn toggle_pinned_translation(&mut self, cx: &mut Context<Self>) {
        if self.pinned_translation.is_none() {
            self.reset_translation_scroll(true);
            self.pinned_translation = Some(TranslationCardState::new(
                SharedString::default(),
                TranslationStatus::Idle,
                None,
                self.translation_request_generation,
            ));
        } else {
            self.pinned_translation = None;
        }
        cx.notify();
    }

    pub(super) fn change_target_language(
        &mut self,
        target_language: String,
        source: &Entity<TargetLanguageSelectState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(target_language) = LanguageCode::new(target_language) else {
            return;
        };
        if self.target_language == target_language.as_str() {
            if cx
                .global::<LibraryStore>()
                .document_target_language(&self.path)
                .is_none()
                && let Err(error) = cx
                    .global_mut::<LibraryStore>()
                    .set_document_target_language(&self.path, target_language.as_str())
            {
                eprintln!("failed to save document translation language: {error:#}");
            }
            return;
        }
        if self.translation_session_id.is_some() {
            let previous_language = self.target_language.clone();
            let source = source.clone();
            let answer = window.prompt(
                PromptLevel::Warning,
                "Restart document translation?",
                Some(
                    "Changing the target language discards the current in-memory result and restarts translation.",
                ),
                &[
                    PromptButton::ok("Restart translation"),
                    PromptButton::cancel("Keep current language"),
                ],
                cx,
            );
            cx.spawn_in(window, async move |this, cx| {
                let restart = answer.await == Ok(0);
                _ = this.update_in(cx, |this, window, cx| {
                    if restart {
                        this.apply_target_language(target_language, &source, window, cx);
                    } else {
                        source.update(cx, |select, cx| {
                            select.set_selected_value(&previous_language, window, cx);
                        });
                    }
                });
            })
            .detach();
            return;
        }
        self.apply_target_language(target_language, source, window, cx);
    }

    fn apply_target_language(
        &mut self,
        target_language: LanguageCode,
        source: &Entity<TargetLanguageSelectState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.target_language = target_language.as_str().to_string();
        for select in [
            self.translation_popup_language_select.as_ref(),
            self.pinned_translation_language_select.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if select.state.entity_id() == source.entity_id() {
                continue;
            }
            let value = target_language.as_str().to_string();
            select.state.update(cx, |select, cx| {
                select.set_selected_value(&value, window, cx);
            });
        }
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .set_document_target_language(&self.path, target_language.as_str())
        {
            eprintln!("failed to save document translation language: {error:#}");
        }

        let popup = self
            .translation_popup
            .as_ref()
            .map(|state| (state.source.to_string(), state.position));
        let pinned_source = self
            .pinned_translation
            .as_ref()
            .map(|state| state.source.to_string());

        match (popup, pinned_source) {
            (Some((source, position)), Some(pinned_source))
                if !source.is_empty() && source == pinned_source =>
            {
                self.translate_text(source, position, true, cx);
            }
            (popup, pinned_source) => {
                if let Some((source, position)) = popup {
                    self.translate_text(source, position, false, cx);
                }
                if let Some(source) = pinned_source
                    && !source.is_empty()
                {
                    self.translate_text(source, None, true, cx);
                }
            }
        }
        if let Some(id) = self.translation_session_id {
            self.document_translation_jobs.update(cx, |manager, cx| {
                manager.restart_session_in_language(id, target_language, cx);
            });
        }
        cx.notify();
    }

    fn close_translation_popup(&mut self, cx: &mut Context<Self>) {
        if self.translation_popup.take().is_some() {
            cx.notify();
        }
    }

    fn translation_panel_local_pointer(&self, position: GpuiPoint<Pixels>) -> GpuiPoint<Pixels> {
        point(
            position.x - self.viewport_origin.x,
            position.y - self.viewport_origin.y,
        )
    }

    fn translation_panel_effective_size(&self) -> Size<Pixels> {
        size(
            px(translation_panel_width(self.viewport_width)),
            px(self
                .translation_panel_size
                .height
                .as_f32()
                .max(TRANSLATION_PANEL_DEFAULT_HEIGHT)),
        )
    }

    fn clamp_translation_panel_placement(&mut self) {
        let panel_size = self.translation_panel_effective_size();
        self.translation_panel_placement = match self.translation_panel_placement {
            TranslationPanelPlacement::Default => TranslationPanelPlacement::Default,
            TranslationPanelPlacement::Floating(position) => {
                TranslationPanelPlacement::Floating(clamp_translation_panel_position(
                    position,
                    self.viewport_width,
                    self.viewport_height,
                    panel_size,
                ))
            }
            TranslationPanelPlacement::PendingDock { position, side, y } => {
                let position = clamp_translation_panel_position(
                    position,
                    self.viewport_width,
                    self.viewport_height,
                    panel_size,
                );
                TranslationPanelPlacement::PendingDock {
                    position,
                    side,
                    y: clamp_translation_panel_edge_y(
                        y,
                        self.viewport_height,
                        panel_size.height.as_f32(),
                    ),
                }
            }
            TranslationPanelPlacement::Docked { side, y, reveal } => {
                TranslationPanelPlacement::Docked {
                    side,
                    y: clamp_translation_panel_edge_y(
                        y,
                        self.viewport_height,
                        panel_size.height.as_f32(),
                    ),
                    reveal,
                }
            }
        };
    }

    fn cancel_translation_panel_pending_dock(&mut self) {
        self.translation_panel_dock_generation =
            self.translation_panel_dock_generation.wrapping_add(1);
        self.translation_panel_dock_task = None;
        if let TranslationPanelPlacement::PendingDock { position, .. } =
            self.translation_panel_placement
        {
            self.translation_panel_placement = TranslationPanelPlacement::Floating(position);
        }
    }

    fn cancel_translation_panel_initial_dock(&mut self) {
        if self.translation_panel_initial_dock_task.is_none() {
            return;
        }
        self.translation_panel_initial_dock_generation = self
            .translation_panel_initial_dock_generation
            .wrapping_add(1);
        self.translation_panel_initial_dock_task = None;
    }

    fn translation_panel_initial_dock_eligible(&self, cx: &gpui::App) -> bool {
        translation_panel_initial_dock_should_run(
            self.translation_panel_initial_dock_armed,
            self.translation_view_role,
            self.translation_pair_active,
            self.translation_session_snapshot.is_some(),
            matches!(
                self.translation_panel_placement,
                TranslationPanelPlacement::Default
            ),
            self.translation_panel_drag.is_some(),
            self.translation_panel_dock_panel_hovered,
            self.translation_panel_owned_surface_active(cx),
        )
    }

    fn translation_panel_owned_surface_active(&self, cx: &gpui::App) -> bool {
        self.pinned_translation_language_select
            .as_ref()
            .is_some_and(|select| select.is_open(cx))
    }

    pub(super) fn sync_translation_panel_initial_dock(&mut self, cx: &mut Context<Self>) {
        if !self.translation_panel_initial_dock_eligible(cx) {
            self.cancel_translation_panel_initial_dock();
            return;
        }
        if self.translation_panel_initial_dock_task.is_some() {
            return;
        }
        self.translation_panel_initial_dock_generation = self
            .translation_panel_initial_dock_generation
            .wrapping_add(1);
        let generation = self.translation_panel_initial_dock_generation;
        self.translation_panel_initial_dock_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(TRANSLATION_PANEL_INITIAL_DOCK_DELAY)
                .await;
            _ = this.update(cx, |this, cx| {
                if this.translation_panel_initial_dock_generation != generation {
                    return;
                }
                this.translation_panel_initial_dock_task = None;
                if !this.translation_panel_initial_dock_eligible(cx) {
                    return;
                }
                let now = Instant::now();
                let reduce_motion = cx.global::<SettingsStore>().settings.behavior.reduce_motion;
                let mut reveal = ScalarTween::new(1.0, LAYOUT);
                reveal.retarget(0.0, now, reduce_motion);
                this.translation_panel_placement = TranslationPanelPlacement::Docked {
                    side: TranslationPanelSide::Right,
                    y: px(TRANSLATION_PANEL_DEFAULT_TOP),
                    reveal,
                };
                this.translation_panel_initial_dock_armed = false;
                this.translation_panel_dock_panel_hovered = false;
                this.translation_panel_dock_edge_hovered = false;
                cx.notify();
            });
        }));
    }

    fn cancel_translation_panel_retract(&mut self) {
        self.translation_panel_retract_generation =
            self.translation_panel_retract_generation.wrapping_add(1);
        self.translation_panel_retract_task = None;
    }

    fn schedule_translation_panel_dock(&mut self, cx: &mut Context<Self>) {
        self.translation_panel_dock_generation =
            self.translation_panel_dock_generation.wrapping_add(1);
        self.translation_panel_dock_task = None;
        let generation = self.translation_panel_dock_generation;
        self.translation_panel_dock_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(TRANSLATION_PANEL_DOCK_DELAY)
                .await;
            _ = this.update(cx, |this, cx| {
                if this.translation_panel_dock_generation != generation {
                    return;
                }
                let TranslationPanelPlacement::PendingDock { side, y, .. } =
                    this.translation_panel_placement
                else {
                    return;
                };
                let now = Instant::now();
                let reduce_motion = cx.global::<SettingsStore>().settings.behavior.reduce_motion;
                let mut reveal = ScalarTween::new(1.0, LAYOUT);
                if !this.translation_panel_owned_surface_active(cx) {
                    reveal.retarget(0.0, now, reduce_motion);
                }
                this.translation_panel_placement =
                    TranslationPanelPlacement::Docked { side, y, reveal };
                this.translation_panel_dock_task = None;
                this.translation_panel_dock_panel_hovered = false;
                this.translation_panel_dock_edge_hovered = false;
                cx.notify();
            });
        }));
    }

    fn translation_panel_should_stay_expanded(&self, cx: &gpui::App) -> bool {
        translation_panel_should_retain_expanded(
            self.translation_panel_dock_panel_hovered,
            self.translation_panel_dock_edge_hovered,
            self.translation_panel_owned_surface_active(cx),
        )
    }

    fn expand_translation_panel_dock(&mut self, cx: &mut Context<Self>) {
        self.cancel_translation_panel_retract();
        if let TranslationPanelPlacement::Docked { reveal, .. } =
            &mut self.translation_panel_placement
        {
            reveal.retarget(
                1.0,
                Instant::now(),
                cx.global::<SettingsStore>().settings.behavior.reduce_motion,
            );
            cx.notify();
        }
    }

    fn schedule_translation_panel_retract(&mut self, cx: &mut Context<Self>) {
        if self.translation_panel_retract_task.is_some()
            || self.translation_panel_should_stay_expanded(cx)
        {
            return;
        }
        self.translation_panel_retract_generation =
            self.translation_panel_retract_generation.wrapping_add(1);
        let generation = self.translation_panel_retract_generation;
        self.translation_panel_retract_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(TRANSLATION_PANEL_RETRACT_DELAY)
                .await;
            _ = this.update(cx, |this, cx| {
                if this.translation_panel_retract_generation != generation
                    || this.translation_panel_should_stay_expanded(cx)
                {
                    return;
                }
                if let TranslationPanelPlacement::Docked { reveal, .. } =
                    &mut this.translation_panel_placement
                {
                    reveal.retarget(
                        0.0,
                        Instant::now(),
                        cx.global::<SettingsStore>().settings.behavior.reduce_motion,
                    );
                    this.translation_panel_retract_task = None;
                    cx.notify();
                }
            });
        }));
    }

    pub(super) fn sync_translation_panel_owned_surfaces(&mut self, cx: &mut Context<Self>) {
        self.sync_translation_panel_initial_dock(cx);
        if !matches!(
            self.translation_panel_placement,
            TranslationPanelPlacement::Docked { .. }
        ) {
            return;
        }
        if self.translation_panel_should_stay_expanded(cx) {
            self.expand_translation_panel_dock(cx);
        } else {
            self.schedule_translation_panel_retract(cx);
        }
    }

    pub(super) fn set_translation_panel_dock_surface_hovered(
        &mut self,
        surface: TranslationPanelDockSurface,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        let current = match surface {
            TranslationPanelDockSurface::Panel => &mut self.translation_panel_dock_panel_hovered,
            TranslationPanelDockSurface::Edge => &mut self.translation_panel_dock_edge_hovered,
        };
        if *current == hovered {
            return;
        }
        *current = hovered;
        if !matches!(
            self.translation_panel_placement,
            TranslationPanelPlacement::Docked { .. }
        ) {
            if surface == TranslationPanelDockSurface::Edge {
                self.translation_panel_dock_edge_hovered = false;
            }
            self.sync_translation_panel_initial_dock(cx);
            return;
        }
        if hovered || self.translation_panel_should_stay_expanded(cx) {
            self.expand_translation_panel_dock(cx);
        } else {
            self.schedule_translation_panel_retract(cx);
        }
    }

    pub(super) fn translation_panel_viewport_changed(&mut self) {
        self.cancel_translation_panel_pending_dock();
        self.cancel_translation_panel_retract();
        self.clamp_translation_panel_placement();
    }

    fn record_translation_panel_size(&mut self, measured: Size<Pixels>, cx: &mut Context<Self>) {
        let changed = (self.translation_panel_size.width - measured.width)
            .as_f32()
            .abs()
            > 0.5
            || (self.translation_panel_size.height - measured.height)
                .as_f32()
                .abs()
                > 0.5;
        if !changed {
            return;
        }
        self.translation_panel_size = measured;
        self.clamp_translation_panel_placement();
        cx.notify();
    }

    pub(super) fn begin_translation_panel_drag(
        &mut self,
        pointer: GpuiPoint<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let pointer = self.translation_panel_local_pointer(pointer);
        self.translation_panel_initial_dock_armed = false;
        self.cancel_translation_panel_initial_dock();
        let panel_size = self.translation_panel_effective_size();
        let now = Instant::now();
        let origin = match self.translation_panel_placement {
            TranslationPanelPlacement::Default => {
                default_translation_panel_position(self.viewport_width, panel_size.width.as_f32())
            }
            TranslationPanelPlacement::Floating(position) => position,
            TranslationPanelPlacement::PendingDock { position, .. } => position,
            TranslationPanelPlacement::Docked {
                side, y, reveal, ..
            } => point(
                translation_panel_docked_x(
                    side,
                    reveal.value(now),
                    self.viewport_width,
                    panel_size.width.as_f32(),
                ),
                y,
            ),
        };
        self.cancel_translation_panel_pending_dock();
        self.cancel_translation_panel_retract();
        let origin = clamp_translation_panel_position(
            origin,
            self.viewport_width,
            self.viewport_height,
            panel_size,
        );
        self.translation_panel_placement = TranslationPanelPlacement::Floating(origin);
        self.translation_panel_drag = Some(TranslationPanelDrag::Floating {
            pointer_offset: point(pointer.x - origin.x, pointer.y - origin.y),
        });
        self.translation_panel_dock_panel_hovered = false;
        self.translation_panel_dock_edge_hovered = false;
        cx.notify();
    }

    pub(super) fn update_translation_panel_drag(
        &mut self,
        pointer: GpuiPoint<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(drag) = self.translation_panel_drag else {
            return false;
        };
        let pointer = self.translation_panel_local_pointer(pointer);
        let panel_size = self.translation_panel_effective_size();
        let TranslationPanelDrag::Floating { pointer_offset } = drag;
        let position = clamp_translation_panel_position(
            point(pointer.x - pointer_offset.x, pointer.y - pointer_offset.y),
            self.viewport_width,
            self.viewport_height,
            panel_size,
        );
        self.translation_panel_placement = TranslationPanelPlacement::Floating(position);
        cx.notify();
        true
    }

    pub(super) fn finish_translation_panel_drag(
        &mut self,
        _pointer: GpuiPoint<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(drag) = self.translation_panel_drag.take() else {
            return false;
        };
        let TranslationPanelDrag::Floating { .. } = drag;
        let panel_size = self.translation_panel_effective_size();
        if let TranslationPanelPlacement::Floating(position) = self.translation_panel_placement
            && let Some(side) = translation_panel_snap_side(
                position,
                panel_size.width.as_f32(),
                self.viewport_width,
            )
        {
            let y = clamp_translation_panel_edge_y(
                position.y,
                self.viewport_height,
                panel_size.height.as_f32(),
            );
            self.translation_panel_placement =
                TranslationPanelPlacement::PendingDock { position, side, y };
            self.schedule_translation_panel_dock(cx);
        } else {
            self.cancel_translation_panel_pending_dock();
        }
        cx.notify();
        true
    }

    pub(super) fn render_translation_session_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !translation_toolbar_enabled(self.translation_view_role, self.translation_pair_active) {
            return None;
        }
        let session_id = self.translation_session_id?;
        if self.translation_pair_session_id != Some(session_id) {
            return None;
        }
        let snapshot = self.translation_session_snapshot.clone()?;
        if snapshot.id != session_id {
            return None;
        }
        let panel_width = translation_panel_width(self.viewport_width);
        let compact = panel_width < TRANSLATION_PANEL_COMPACT_BREAKPOINT;
        self.clamp_translation_panel_placement();

        let (status, detail) = match &snapshot.status {
            DocumentTranslationSessionStatus::Queued => {
                ("Queued".to_string(), "Waiting to start".to_string())
            }
            DocumentTranslationSessionStatus::Running { progress, units } => {
                if let Some(progress) = progress {
                    let status = match progress.stage {
                        glyphst_pdf_translation_app::PdfTranslationStage::Preparing => "Preparing",
                        glyphst_pdf_translation_app::PdfTranslationStage::Ocr => "OCR",
                        glyphst_pdf_translation_app::PdfTranslationStage::Normalizing => {
                            "Preparing pages"
                        }
                        glyphst_pdf_translation_app::PdfTranslationStage::Translating => {
                            "Translating"
                        }
                        glyphst_pdf_translation_app::PdfTranslationStage::Layout
                        | glyphst_pdf_translation_app::PdfTranslationStage::Compiling
                        | glyphst_pdf_translation_app::PdfTranslationStage::Composing => "Preview",
                        glyphst_pdf_translation_app::PdfTranslationStage::Validating => {
                            "Validating"
                        }
                    };
                    (
                        status.to_string(),
                        format!(
                            "Page {} / {} ready · {} previewed · {}",
                            snapshot.ready_prefix.min(self.page_count),
                            self.page_count,
                            snapshot.page_previews.len(),
                            progress.detail
                        ),
                    )
                } else if units.total > 0 {
                    (
                        "Translating".to_string(),
                        format!(
                            "Page {} / {} · {} / {} units · {} retrying · {} failed",
                            snapshot.ready_prefix.min(self.page_count),
                            self.page_count,
                            units.translated + units.cached,
                            units.total,
                            units.retrying,
                            units.failed
                        ),
                    )
                } else {
                    (
                        "Preparing".to_string(),
                        format!(
                            "Page {} / {} ready",
                            snapshot.ready_prefix.min(self.page_count),
                            self.page_count
                        ),
                    )
                }
            }
            DocumentTranslationSessionStatus::Cancelling { .. } => {
                ("Cancelling".to_string(), "Stopping translation".to_string())
            }
            DocumentTranslationSessionStatus::Paused {
                units,
                cache_available,
                ..
            } => {
                let resolved = units.translated + units.cached;
                let detail = if !cache_available {
                    "Cached pages unavailable · Resume to rebuild".to_string()
                } else if units.total > 0 {
                    format!("{resolved} / {} units cached", units.total)
                } else {
                    "Ready to resume".to_string()
                };
                ("Paused".to_string(), detail)
            }
            DocumentTranslationSessionStatus::Cancelled => {
                ("Cancelled".to_string(), "Translation stopped".to_string())
            }
            DocumentTranslationSessionStatus::Completed { .. }
                if snapshot.last_export_error.is_some() =>
            {
                (
                    "Save failed".to_string(),
                    snapshot
                        .last_export_error
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                )
            }
            DocumentTranslationSessionStatus::Completed { failed_units: 0 } => {
                ("Complete".to_string(), "Ready to save".to_string())
            }
            DocumentTranslationSessionStatus::Completed { failed_units } => (
                "Complete with warnings".to_string(),
                format!("{failed_units} unit(s) remain untranslated"),
            ),
            DocumentTranslationSessionStatus::Exporting { progress, .. } => (
                "Saving".to_string(),
                progress
                    .as_ref()
                    .map(|progress| progress.detail.clone())
                    .unwrap_or_else(|| "Preparing the output PDF".to_string()),
            ),
            DocumentTranslationSessionStatus::Failed { message, .. } => {
                ("Failed".to_string(), message.to_string())
            }
        };
        let language_select = self.pinned_translation_language_select.clone();
        let can_save = snapshot.can_save();
        let can_resume = snapshot.can_resume();
        let can_retry = snapshot.has_failures() && !snapshot.is_translating() && !can_resume;

        let drag_token = TranslationPanelDragToken(self.doc_id);
        let grip = div()
            .id("translation-session-drag-surface")
            .flex_none()
            .size(px(24.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_grab()
            .hover(|this| this.bg(cx.theme().secondary_hover))
            .tooltip(text_tooltip("Move translation tools"))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.begin_translation_panel_drag(event.position, cx);
                    cx.stop_propagation();
                }),
            )
            .on_drag(drag_token, |drag, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| drag.clone())
            })
            .child(
                Icon::new(IconName::GripVertical)
                    .small()
                    .text_color(cx.theme().muted_foreground),
            );
        let language = div()
            .min_w_0()
            .flex_1()
            .when_some(language_select, |this, select| {
                this.child(
                    Select::new(&select.state)
                        .small()
                        .w_full()
                        .max_w_full()
                        .menu_width(px(190.0))
                        .menu_max_h(px(260.0))
                        .menu_priority(TRANSLATION_PANEL_POPOVER_PRIORITY)
                        .search_placeholder("Search languages"),
                )
            });
        let (progress_value, progress_indeterminate) = match &snapshot.status {
            DocumentTranslationSessionStatus::Running { progress, units } => {
                if let Some(progress) = progress {
                    (
                        (progress.overall_fraction.is_finite() && !progress.indeterminate)
                            .then_some(progress.overall_fraction.clamp(0.0, 1.0)),
                        progress.indeterminate || !progress.overall_fraction.is_finite(),
                    )
                } else if units.total > 0 {
                    (
                        Some(
                            ((units.translated + units.cached) as f32 / units.total as f32)
                                .clamp(0.0, 1.0),
                        ),
                        false,
                    )
                } else {
                    (None, true)
                }
            }
            DocumentTranslationSessionStatus::Cancelling { progress }
            | DocumentTranslationSessionStatus::Exporting { progress, .. } => progress
                .as_ref()
                .map(|progress| {
                    (
                        (progress.overall_fraction.is_finite() && !progress.indeterminate)
                            .then_some(progress.overall_fraction.clamp(0.0, 1.0)),
                        progress.indeterminate || !progress.overall_fraction.is_finite(),
                    )
                })
                .unwrap_or((None, true)),
            DocumentTranslationSessionStatus::Queued => (None, true),
            _ => (None, false),
        };
        let active_progress = snapshot.is_translating()
            || matches!(
                &snapshot.status,
                DocumentTranslationSessionStatus::Exporting { .. }
            );
        let cancelling = matches!(
            &snapshot.status,
            DocumentTranslationSessionStatus::Cancelling { .. }
        );
        let status_tooltip = format!("{status}: {detail}");
        let terminal_status_icon = match &snapshot.status {
            DocumentTranslationSessionStatus::Cancelled => Some((
                IconName::Stop,
                status_tooltip.clone(),
                cx.theme().muted_foreground,
            )),
            DocumentTranslationSessionStatus::Failed { .. } => Some((
                IconName::TriangleAlert,
                status_tooltip.clone(),
                cx.theme().warning,
            )),
            DocumentTranslationSessionStatus::Completed { .. }
                if snapshot.last_export_error.is_some() =>
            {
                Some((
                    IconName::TriangleAlert,
                    status_tooltip.clone(),
                    cx.theme().warning,
                ))
            }
            _ => None,
        };
        let actions = div()
            .flex_none()
            .flex()
            .flex_nowrap()
            .items_center()
            .gap_1()
            .child(
                toolbar_icon_button(
                    format!("translation-session-{session_id}-toggle-overlays"),
                    IconName::Scan,
                )
                .debug_selector(|| "translation-session-toggle-overlays".to_string())
                .selected(self.translation_overlay_enabled)
                .tooltip(if self.translation_overlay_enabled {
                    "Hide paired block overlays"
                } else {
                    "Show paired block overlays"
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_translation_overlay(cx);
                    cx.stop_propagation();
                })),
            )
            .when(active_progress, |this| {
                let indicator = ProgressCircle::new(format!(
                    "translation-session-{session_id}-progress-circle"
                ))
                .loading(progress_indeterminate)
                .value(progress_value.unwrap_or_default() * 100.0)
                .child(Icon::new(IconName::X).xsmall());
                this.child(
                    toolbar_icon_button(
                        format!("translation-session-{session_id}-progress"),
                        IconName::X,
                    )
                    .icon(indicator)
                    .disabled(cancelling)
                    .tooltip(status_tooltip.clone())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.cancel_document_translation(cx);
                        cx.stop_propagation();
                    })),
                )
            })
            .when(can_resume, |this| {
                this.child(
                    toolbar_icon_button(
                        format!("translation-session-{session_id}-resume"),
                        IconName::Play,
                    )
                    .tooltip(status_tooltip.clone())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.resume_translation_session(cx);
                        cx.stop_propagation();
                    })),
                )
            })
            .when(can_retry, |this| {
                this.child(
                    toolbar_icon_button(
                        format!("translation-session-{session_id}-retry"),
                        IconName::RotateCcw,
                    )
                    .tooltip("Retry failed units")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.retry_failed_translation_units(cx);
                        cx.stop_propagation();
                    })),
                )
            })
            .when(can_save, |this| {
                this.child(
                    toolbar_icon_button(
                        format!("translation-session-{session_id}-save"),
                        IconName::Download,
                    )
                    .tooltip("Save translation")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.request_save_translation_session(cx);
                        cx.stop_propagation();
                    })),
                )
            })
            .when_some(terminal_status_icon, |this, (icon, tooltip, color)| {
                this.child(
                    div()
                        .id(format!("translation-session-{session_id}-terminal-status"))
                        .size(px(24.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .tooltip(text_tooltip(tooltip))
                        .child(Icon::new(icon).small().text_color(color)),
                )
            });
        let rows = if compact {
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_nowrap()
                        .items_center()
                        .gap_1()
                        .child(grip)
                        .child(language),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_nowrap()
                        .items_center()
                        .justify_end()
                        .gap_1()
                        .child(actions),
                )
                .into_any_element()
        } else {
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_nowrap()
                .items_center()
                .gap_1()
                .child(grip)
                .child(language)
                .child(actions)
                .into_any_element()
        };
        let panel_owner = cx.entity().downgrade();
        let panel = div()
            .id("translation-session-panel")
            .w(px(panel_width))
            .max_w_full()
            .flex()
            .flex_col()
            .p_2()
            .rounded_lg()
            .popover_style(cx)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.set_translation_panel_dock_surface_hovered(
                    TranslationPanelDockSurface::Panel,
                    *hovered,
                    cx,
                );
            }))
            .child(rows)
            .on_prepaint(move |bounds, _, cx| {
                if let Some(panel_owner) = panel_owner.upgrade() {
                    panel_owner.update(cx, |this, cx| {
                        this.record_translation_panel_size(bounds.size, cx);
                    });
                }
            });
        if let TranslationPanelPlacement::Docked { side, y, reveal } =
            self.translation_panel_placement
        {
            let now = Instant::now();
            if reveal.is_animating(now) {
                window.request_animation_frame();
            }
            let panel_height = self.translation_panel_effective_size().height;
            let panel_x = translation_panel_docked_x(
                side,
                reveal.value(now),
                self.viewport_width,
                panel_width,
            );
            let edge_surface = div()
                .id(format!(
                    "translation-session-{session_id}-edge-hover-surface"
                ))
                .absolute()
                .top(y)
                .w(px(TRANSLATION_PANEL_EXPOSED_STRIP))
                .h(panel_height)
                .when(side == TranslationPanelSide::Left, |this| this.left_0())
                .when(side == TranslationPanelSide::Right, |this| this.right_0())
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    this.set_translation_panel_dock_surface_hovered(
                        TranslationPanelDockSurface::Edge,
                        *hovered,
                        cx,
                    );
                }));
            let positioned_panel = div().absolute().left(panel_x).top(y).child(panel);
            return Some(
                deferred(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .overflow_hidden()
                        .child(positioned_panel)
                        .child(edge_surface),
                )
                .with_priority(TRANSLATION_PANEL_OVERLAY_PRIORITY)
                .into_any_element(),
            );
        }
        let dragged_position = match self.translation_panel_placement {
            TranslationPanelPlacement::Floating(position) => Some(position),
            TranslationPanelPlacement::PendingDock { position, .. } => Some(position),
            TranslationPanelPlacement::Default => None,
            TranslationPanelPlacement::Docked { .. } => unreachable!(),
        };
        let mut overlay = div()
            .absolute()
            .top(
                dragged_position
                    .map(|position| position.y)
                    .unwrap_or_else(|| px(TRANSLATION_PANEL_DEFAULT_TOP)),
            )
            .max_w_full();
        overlay = if let Some(position) = dragged_position {
            overlay.left(position.x)
        } else {
            // Both edges are resolved against this document pane. The flex
            // wrapper constrains the preferred-width panel even on the first
            // frame, before the canvas has reported its viewport size.
            overlay
                .left(px(TRANSLATION_PANEL_MARGIN))
                .right(px(TRANSLATION_PANEL_DEFAULT_RIGHT))
                .flex()
                .justify_end()
        };
        let overlay_priority = if self.translation_panel_drag.is_some() {
            // The settled panel belongs below root notifications. During a
            // drag, temporarily lift the source surface so a notification at
            // the default top-right position cannot swallow move or mouse-up
            // events and strand the panel on the other side of the notice.
            TRANSLATION_PANEL_DRAG_OVERLAY_PRIORITY
        } else {
            TRANSLATION_PANEL_OVERLAY_PRIORITY
        };
        Some(
            deferred(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .child(overlay.child(panel)),
            )
            .with_priority(overlay_priority)
            .into_any_element(),
        )
    }

    pub(super) fn render_translation_hover_tools(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !translation_toolbar_enabled(self.translation_view_role, self.translation_pair_active)
            || !self.translation_overlay_enabled
        {
            return None;
        }
        let snapshot = self.translation_session_snapshot.as_ref()?;
        let region_id = self.hovered_translation_region.as_ref()?.clone();
        let region = self.translation_hover_regions.get(&region_id)?;
        let unit_id = region.unit_id.clone();
        let failed = self.translation_failed_hover_regions.contains(&region_id);
        if unit_id.is_none() && !failed {
            return None;
        }
        let now = Instant::now();
        let bounds = self
            .translation_hover_animation
            .as_ref()
            .filter(|animation| animation.region_id == region_id)
            .map(|animation| animation.bounds(now))
            .or_else(|| {
                self.translation_hover_region_highlights(&region_id)
                    .into_iter()
                    .reduce(|left, right| left.union(&right))
            })?;
        let position = point(bounds.right(), bounds.bottom() + px(4.0));
        let using_original = unit_id
            .as_ref()
            .is_some_and(|unit_id| snapshot.original_unit_ids.contains(unit_id));
        let toggle_unit_id = unit_id.clone();
        let toggle_region_id = region_id.clone();
        let retry_region_id = region_id.clone();
        let tools_owner = cx.entity().downgrade();
        let measured_region_id = region_id.clone();
        Some(
            deferred(
                anchored()
                    .snap_to_window_with_margin(px(8.0))
                    .position(position)
                    .anchor(Anchor::TopRight)
                    .child(
                        div()
                            .id(format!("translation-region-{region_id}-tools"))
                            .flex()
                            .flex_row_reverse()
                            .items_center()
                            .gap(px(2.0))
                            .p(px(2.0))
                            .rounded_lg()
                            .popover_style(cx)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                                this.set_translation_hover_tools_hovered(*hovered, cx);
                            }))
                            .on_prepaint(move |bounds, _, cx| {
                                if let Some(tools_owner) = tools_owner.upgrade() {
                                    tools_owner.update(cx, |this, _| {
                                        this.record_translation_hover_tools_bounds(
                                            measured_region_id.clone(),
                                            bounds,
                                        );
                                    });
                                }
                            })
                            .when_some(toggle_unit_id, |this, unit_id| {
                                let clicked_unit_id = unit_id.clone();
                                this.child(
                                    toolbar_icon_button(
                                        format!(
                                            "translation-region-{toggle_region_id}-{unit_id}-toggle"
                                        ),
                                        IconName::Repeat,
                                    )
                                    .w(px(20.0))
                                    .h(px(20.0))
                                    .tooltip(if using_original {
                                        "Use translated text"
                                    } else {
                                        "Use original text"
                                    })
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.toggle_translation_unit(&clicked_unit_id, cx);
                                            cx.stop_propagation();
                                        },
                                    )),
                                )
                            })
                            .when(failed, |this| {
                                this.child(
                                    toolbar_icon_button(
                                        format!("translation-region-{retry_region_id}-retry"),
                                        IconName::RotateCcw,
                                    )
                                    .w(px(20.0))
                                    .h(px(20.0))
                                    .tooltip("Retry failed translation")
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.retry_failed_translation_units(cx);
                                            cx.stop_propagation();
                                        },
                                    )),
                                )
                            }),
                    ),
            )
            .with_priority(TRANSLATION_HOVER_TOOLS_OVERLAY_PRIORITY)
            .into_any_element(),
        )
    }

    pub(super) fn translation_hover_region_highlights(
        &self,
        region_id: &str,
    ) -> Vec<Bounds<Pixels>> {
        let Some(region) = self.translation_hover_regions.get(region_id) else {
            return Vec::new();
        };
        let Some(image_bounds) = self
            .page_image_bounds
            .get(region.page_index)
            .and_then(Option::as_ref)
            .copied()
        else {
            return Vec::new();
        };
        let bounds = match self.translation_view_role {
            TranslationViewRole::Translation => std::slice::from_ref(&region.target_bounds),
            TranslationViewRole::SourceMirror => region.source_regions.as_slice(),
            TranslationViewRole::None => &[],
        };
        bounds
            .iter()
            .map(|bounds| {
                self.pdf_rect_to_screen(
                    region.page_index,
                    Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1),
                    image_bounds,
                )
            })
            .collect()
    }

    pub(super) fn translation_hover_region_at_screen(
        &self,
        position: GpuiPoint<Pixels>,
    ) -> Option<String> {
        if self.translation_view_role != TranslationViewRole::Translation
            || !self.translation_overlay_enabled
        {
            return None;
        }
        self.translation_hover_regions
            .keys()
            .filter_map(|region_id| {
                self.translation_hover_region_highlights(region_id)
                    .iter()
                    .copied()
                    .reduce(|left, right| left.union(&right))
                    .filter(|bounds| bounds.contains(&position))
                    .map(|bounds| {
                        (
                            region_id,
                            bounds.size.width.as_f32() * bounds.size.height.as_f32(),
                        )
                    })
            })
            .min_by(|(left_id, left_area), (right_id, right_area)| {
                left_area
                    .total_cmp(right_area)
                    .then_with(|| left_id.cmp(right_id))
            })
            .map(|(region_id, _)| region_id.clone())
    }

    pub(super) fn render_translation_card(
        &self,
        state: TranslationCardState,
        pinned: bool,
        theme: ThemeColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let language_select = if pinned {
            self.pinned_translation_language_select.clone()
        } else {
            self.translation_popup_language_select.clone()
        };
        let language_select_open = language_select
            .as_ref()
            .is_some_and(|select| select.is_open(cx));
        let font = &cx.global::<SettingsStore>().settings.font;
        let base_text_style = TextStyle {
            font_family: font.ui_font_family.clone().into(),
            font_size: ui_rems(13.0).into(),
            color: theme.foreground,
            font_weight: FontWeight(font.ui_font_weight),
            ..Default::default()
        };
        let content = match &state.status {
            TranslationStatus::Idle => div().into_any_element(),
            TranslationStatus::Loading => div()
                .py_2()
                .text_color(theme.muted_foreground)
                .child(crate::fl!("translation-loading"))
                .into_any_element(),
            TranslationStatus::Ready { markdown, .. } => div()
                .w_full()
                .min_w_0()
                .child(MarkdownElement::new(
                    markdown.clone(),
                    MarkdownStyle::from_theme(theme, base_text_style),
                ))
                .into_any_element(),
            TranslationStatus::Error(error) => Alert::error(
                if pinned {
                    "pinned-translation-error"
                } else {
                    "translation-popup-error"
                },
                error.clone(),
            )
            .xsmall()
            .into_any_element(),
        };
        let copy_text = match &state.status {
            TranslationStatus::Ready { text, .. } => Some(text.clone()),
            _ => None,
        };
        let scrollable = matches!(
            state.status,
            TranslationStatus::Ready { .. } | TranslationStatus::Error(_)
        );
        let scroll_handle = if pinned {
            &self.pinned_translation_scroll
        } else {
            &self.translation_popup_scroll
        };
        let scrollbar_id = if pinned {
            "pinned-translation-scrollbar"
        } else {
            "translation-popup-scrollbar"
        };

        div()
            .occlude()
            .w(px(cx
                .global::<SettingsStore>()
                .settings
                .translation
                .pinned
                .width))
            .overflow_hidden()
            .flex()
            .flex_col()
            .popover_style(cx)
            .text_size(ui_rems(13.0))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .when(!pinned && !language_select_open, |this| {
                this.on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.close_translation_popup(cx);
                }))
            })
            .on_scroll_wheel(|_, _, cx| {
                cx.stop_propagation();
            })
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .p_2()
                    .border_b(px(1.0))
                    .border_color(theme.input)
                    .text_size(ui_rems(11.0))
                    .text_color(theme.muted_foreground)
                    .when_some(language_select, |this, select| {
                        this.child(
                            Select::new(&select.state)
                                .small()
                                .w(px(180.0))
                                .menu_width(px(180.0))
                                .menu_max_h(px(240.0))
                                .search_placeholder(crate::fl!(
                                    "settings-search-languages-placeholder"
                                )),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .when_some(copy_text, |this, text| {
                                this.child(
                                    toolbar_icon_button("translation-popup-copy", IconName::Copy)
                                        .tooltip(crate::fl!("copy"))
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                text.to_string(),
                                            ));
                                            cx.stop_propagation();
                                        })),
                                )
                            })
                            .child(
                                toolbar_icon_button("translation-popup-dismiss", IconName::X)
                                    .tooltip(crate::fl!("translation-close"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if pinned {
                                            this.toggle_pinned_translation(cx);
                                        } else {
                                            this.close_translation_popup(cx);
                                        }
                                        cx.stop_propagation();
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .child(
                        div()
                            .id(if pinned {
                                "pinned-translation-content"
                            } else {
                                "translation-popup-content"
                            })
                            .track_scroll(scroll_handle)
                            .w_full()
                            .min_w_0()
                            .max_h(px(300.0))
                            .flex()
                            .flex_col()
                            .overflow_y_scroll()
                            .p_3()
                            .cursor_text()
                            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                            .child(content),
                    )
                    .when(scrollable, |this| {
                        this.vertical_scrollbar_for_with_id(scroll_handle, scrollbar_id, window, cx)
                    }),
            )
            .into_any_element()
    }
}
