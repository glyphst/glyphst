use super::*;

impl DocumentView {
    pub(super) fn diff_flash_overlays(&self, now: Instant) -> Vec<DiffFlashOverlay> {
        let duration = DIFF_FLASH_DURATION.as_secs_f32().max(f32::EPSILON);
        self.diff_flashes
            .iter()
            .filter_map(|flash| {
                if flash.page_idx >= self.pages.len() {
                    return None;
                }

                let elapsed = now.saturating_duration_since(flash.started_at);
                if elapsed >= DIFF_FLASH_DURATION {
                    return None;
                }

                let image_bounds = *self
                    .page_image_bounds
                    .get(flash.page_idx)
                    .and_then(Option::as_ref)?;
                let progress = (elapsed.as_secs_f32() / duration).clamp(0.0, 1.0);
                let alpha = (0x66 as f32 * (1.0 - progress))
                    .round()
                    .clamp(0.0, 0x66 as f32) as u8;

                Some(DiffFlashOverlay {
                    bounds: self.pdf_rect_to_screen(flash.page_idx, flash.rect, image_bounds),
                    alpha,
                })
            })
            .collect()
    }

    fn assemble_paint_data(&mut self, pages: Vec<PaintPage>, window: &mut Window) -> PaintData {
        let (highlight_overlays, selected_highlight_borders) = self.highlight_overlays();
        let note_underlines = self.note_underlines();
        let selection_highlights = self.selection_highlights();
        let (search_highlights, active_search_highlights) = self.search_highlights();
        let link_highlights = self.link_highlights();
        let linked_hover_highlights = self.external_block_highlights(self.external_hovered_block);
        let linked_selection_highlights =
            self.external_block_highlights(self.external_selected_block);
        let now = Instant::now();
        self.diff_flashes
            .retain(|flash| now.saturating_duration_since(flash.started_at) < DIFF_FLASH_DURATION);
        let diff_flash_overlays = self.diff_flash_overlays(now);
        let translated_text_overlays = self.translated_text_overlays(window);
        let region_overlays = self
            .active_region_reference()
            .map_or_else(Vec::new, |reference| self.region_overlay_bounds(&reference));

        PaintData {
            pages,
            translated_text_overlays,
            diff_flash_overlays,
            highlight_overlays,
            selected_highlight_borders,
            note_underlines,
            link_highlights,
            linked_hover_highlights,
            linked_selection_highlights,
            selection_highlights,
            search_highlights,
            active_search_highlights,
            region_overlays,
        }
    }

    pub(super) fn translated_text_overlays(
        &mut self,
        window: &mut Window,
    ) -> Vec<TranslatedTextOverlay> {
        if self.view_language != DocumentViewLanguage::Translated {
            return Vec::new();
        }
        self.translated_text_layout_frame = self.translated_text_layout_frame.wrapping_add(1);
        let frame = self.translated_text_layout_frame;
        let font = window.text_style().font();
        let translated_blocks = self
            .page_block_ranges
            .iter()
            .enumerate()
            .filter(|(page_idx, _)| {
                self.page_image_bounds
                    .get(*page_idx)
                    .is_some_and(Option::is_some)
            })
            .flat_map(|(_, range)| &self.text_blocks[range.clone()])
            .filter_map(|block| self.translated_blocks.get(&block.id))
            .cloned()
            .collect::<Vec<_>>();
        let overlays = translated_blocks
            .iter()
            .filter_map(|block| {
                let image_bounds = *self
                    .page_image_bounds
                    .get(block.source.page_idx)
                    .and_then(Option::as_ref)?;
                let bounds = self.pdf_rect_to_screen(
                    block.source.page_idx,
                    block.source.bounds,
                    image_bounds,
                );
                let scale = bounds.size.height.as_f32()
                    / (block.source.bounds.y1 - block.source.bounds.y0).max(1.0);
                let padding = px(1.0);
                let font_size = px((block.source.font_size * scale * 0.92).max(6.0));
                let key = TranslatedTextLayoutKey {
                    block_id: block.source.id,
                    text: block.text.clone().into(),
                    font: font.clone(),
                    color: block.source.color,
                    font_size_bits: font_size.as_f32().to_bits(),
                    width_bits: bounds.size.width.as_f32().to_bits(),
                    height_bits: bounds.size.height.as_f32().to_bits(),
                };
                let prepared = if let Some(cached) = self.translated_text_layouts.get_mut(&key) {
                    cached.last_used_frame = frame;
                    cached.prepared.clone()
                } else {
                    let prepared = Arc::new(Self::prepare_translated_text(
                        block.text.clone().into(),
                        font.clone(),
                        block.source.color,
                        font_size,
                        bounds,
                        window,
                    )?);
                    self.translated_text_layouts.insert(
                        key,
                        CachedTranslatedTextLayout {
                            prepared: prepared.clone(),
                            last_used_frame: frame,
                        },
                    );
                    prepared
                };
                Some(TranslatedTextOverlay {
                    mask: Bounds::new(
                        point(bounds.left() - padding, bounds.top() - padding),
                        size(
                            bounds.size.width + padding * 2.0,
                            bounds.size.height + padding * 2.0,
                        ),
                    ),
                    bounds,
                    prepared,
                })
            })
            .collect::<Vec<_>>();
        self.prune_translated_text_layouts(frame);
        overlays
    }

    fn prepare_translated_text(
        text: SharedString,
        font: Font,
        color: u32,
        requested_font_size: Pixels,
        bounds: Bounds<Pixels>,
        window: &mut Window,
    ) -> Option<PreparedTranslatedText> {
        let mut font_size = requested_font_size;
        for _ in 0..12 {
            let run = TextRun {
                len: text.len(),
                font: font.clone(),
                color: rgb(color).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let lines = match window.text_system().shape_text(
                text.clone(),
                font_size,
                &[run],
                Some(bounds.size.width),
                None,
            ) {
                Ok(lines) => lines,
                Err(error) => {
                    eprintln!("failed to shape translated PDF text: {error:#}");
                    return None;
                }
            };
            let line_height = font_size * 1.15;
            let height: Pixels = lines.iter().map(|line| line.size(line_height).height).sum();
            if height <= bounds.size.height || font_size <= px(6.0) {
                return Some(PreparedTranslatedText {
                    lines: lines.into_iter().map(Arc::new).collect(),
                    line_height,
                });
            }
            font_size = (font_size * 0.9).max(px(6.0));
        }
        None
    }

    fn prune_translated_text_layouts(&mut self, frame: u64) {
        self.translated_text_layouts.retain(|_, cached| {
            cached.last_used_frame == frame
                || frame.saturating_sub(cached.last_used_frame) <= TRANSLATED_LAYOUT_KEEP_FRAMES
        });
        if self.translated_text_layouts.len() <= MAX_CACHED_TRANSLATED_LAYOUTS {
            return;
        }

        let mut candidates = self
            .translated_text_layouts
            .iter()
            .map(|(key, cached)| (key.clone(), cached.last_used_frame))
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(_, last_used_frame)| *last_used_frame);
        let remove_count = self
            .translated_text_layouts
            .len()
            .saturating_sub(MAX_CACHED_TRANSLATED_LAYOUTS);
        for (key, _) in candidates.into_iter().take(remove_count) {
            self.translated_text_layouts.remove(&key);
        }
    }

    fn presentation_scale_for_page(&self, page_idx: usize) -> f32 {
        if self.pages.get(page_idx).is_none() {
            return self.effective_scale();
        }

        let (page_width, page_height) = self.page_dimensions(page_idx);
        let width_scale = self.viewport_width / page_width.max(1.0);
        let height_scale = self.viewport_height / page_height.max(1.0);
        width_scale.min(height_scale).max(0.001)
    }

    fn presentation_image_bounds_for_page(
        &self,
        page_idx: usize,
        bounds: Bounds<Pixels>,
        scale: f32,
    ) -> Bounds<Pixels> {
        let (page_width, page_height) = self.page_dimensions(page_idx);
        let image_width = px(page_width * scale);
        let image_height = px(page_height * scale);
        let origin = point(
            bounds.origin.x + (bounds.size.width - image_width) / 2.0,
            bounds.origin.y + (bounds.size.height - image_height) / 2.0,
        );

        Bounds::new(origin, size(image_width, image_height))
    }

    fn presentation_paint_data(
        &mut self,
        bounds: Bounds<Pixels>,
        scale_factor: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PaintData {
        let page_idx = self
            .presentation_page
            .unwrap_or_else(|| self.page_index_for_scroll_y(self.scroll_y_doc))
            .min(self.last_page_index());
        self.presentation_page = Some(page_idx);
        self.page_image_bounds.clear();
        self.page_image_bounds
            .resize_with(self.pages.len(), || None);

        let scale = self.presentation_scale_for_page(page_idx);
        let image_bounds = self.presentation_image_bounds_for_page(page_idx, bounds, scale);
        let image_bounds = Self::pixel_aligned_image_bounds(image_bounds, scale_factor);
        let key = self.render_key(page_idx, image_bounds, scale_factor);
        self.page_image_bounds[page_idx] = Some(image_bounds);

        let doc_id = self.doc_id;
        cx.global_mut::<RenderCache>()
            .set_protected_pages([(doc_id, page_idx, self.rotation)]);

        for adjacent_page in [
            page_idx.checked_sub(1),
            (page_idx + 1 < self.pages.len()).then_some(page_idx + 1),
        ]
        .into_iter()
        .flatten()
        {
            let scale = self.presentation_scale_for_page(adjacent_page);
            let image_bounds =
                self.presentation_image_bounds_for_page(adjacent_page, bounds, scale);
            let image_bounds = Self::pixel_aligned_image_bounds(image_bounds, scale_factor);
            let key = self.render_key(adjacent_page, image_bounds, scale_factor);
            if key.pixel_count() <= MAX_PREFETCH_RENDER_PIXELS {
                self.request_render(key, cx);
            }
        }

        let image = self.cached_or_request_render(key, cx);
        self.assemble_paint_data(
            vec![PaintPage {
                bounds: image_bounds,
                image,
            }],
            window,
        )
    }

    pub(super) fn paint_data(
        &mut self,
        bounds: Bounds<Pixels>,
        scale_factor: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<PaintData> {
        let viewport_width = bounds.size.width.as_f32().max(1.0);
        self.viewport_width = viewport_width;
        self.viewport_height = bounds.size.height.as_f32().max(1.0);
        self.schedule_reflow_for_viewport(viewport_width, cx);
        self.base_scale = if self.capabilities.reflow {
            1.0
        } else {
            viewport_width / self.max_page_width.max(1.0)
        };
        if self.is_presentation_mode() {
            return Ok(self.presentation_paint_data(bounds, scale_factor, window, cx));
        }
        self.clamp_scroll();

        self.page_image_bounds.clear();
        self.page_image_bounds
            .resize_with(self.pages.len(), || None);

        let scale = self.effective_scale();
        self.document_scroll.update(
            bounds,
            point(px(0.0), px(-self.scroll_y_doc * scale)),
            point(px(0.0), px(self.max_scroll_y_doc() * scale)),
        );
        let mut visible_pages = Vec::new();
        let viewport_top = bounds.origin.y.as_f32();
        let viewport_bottom = viewport_top + self.viewport_height;

        for page_idx in 0..self.pages.len() {
            let (image_bounds, _) =
                self.image_bounds_and_render_key(page_idx, bounds, scale, scale_factor);
            let page_top = image_bounds.origin.y.as_f32();
            let page_bottom = page_top + image_bounds.size.height.as_f32();

            if page_bottom >= viewport_top && page_top <= viewport_bottom {
                self.page_image_bounds[page_idx] = Some(image_bounds);
                visible_pages.push(page_idx);
            }
        }

        let mut visible_page_renders = Vec::with_capacity(visible_pages.len());
        for page_idx in visible_pages {
            let (image_bounds, key) =
                self.image_bounds_and_render_key(page_idx, bounds, scale, scale_factor);
            visible_page_renders.push((page_idx, image_bounds, key));
        }
        let doc_id = self.doc_id;
        cx.global_mut::<RenderCache>().set_protected_pages(
            visible_page_renders
                .iter()
                .map(|(page_idx, _, key)| (doc_id, *page_idx, key.rotation)),
        );

        if let (Some((first, _, _)), Some((last, _, _))) =
            (visible_page_renders.first(), visible_page_renders.last())
        {
            let start = first.saturating_sub(1);
            let end = (*last + 1).min(self.pages.len().saturating_sub(1));
            for page_idx in start..=end {
                if page_idx >= *first && page_idx <= *last {
                    continue;
                }

                let (_, key) =
                    self.image_bounds_and_render_key(page_idx, bounds, scale, scale_factor);
                if key.pixel_count() <= MAX_PREFETCH_RENDER_PIXELS {
                    self.request_render(key, cx);
                }
            }
        }

        let mut pages = Vec::with_capacity(visible_page_renders.len());
        let mut has_rendered_page = false;
        for (_, image_bounds, key) in visible_page_renders {
            let image = self.cached_or_request_render(key, cx);
            has_rendered_page |= image.is_some();
            pages.push(PaintPage {
                bounds: image_bounds,
                image,
            });
        }
        if has_rendered_page {
            self.start_text_indexing(cx);
        }

        Ok(self.assemble_paint_data(pages, window))
    }
}
