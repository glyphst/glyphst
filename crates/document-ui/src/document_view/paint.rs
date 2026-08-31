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

    fn assemble_paint_data(
        &mut self,
        pages: Vec<PaintPage>,
        reduce_motion: bool,
        window: &mut Window,
    ) -> PaintData {
        let link_invert_overlays = self.link_invert_overlays(&pages);
        let (highlight_overlays, selected_highlight_borders) = self.highlight_overlays();
        let note_underlines = self.note_underlines();
        let selection_highlights = self.selection_highlights();
        let (search_highlights, active_search_highlights) = self.search_highlights();
        let link_highlights = self.link_highlights();
        let linked_hover_highlights = self.translation_hover_highlights(reduce_motion, window);
        let failed_translation_highlights = if translation_toolbar_enabled(
            self.translation.view_role,
            self.translation.pair_active,
        ) && self.translation.overlay_enabled
        {
            self.translation
                .failed_hover_regions
                .iter()
                .flat_map(|region_id| self.translation_hover_region_highlights(region_id))
                .collect()
        } else {
            Vec::new()
        };
        let now = Instant::now();
        self.diff_flashes
            .retain(|flash| now.saturating_duration_since(flash.started_at) < DIFF_FLASH_DURATION);
        let diff_flash_overlays = self.diff_flash_overlays(now);
        let region_overlays = self
            .active_region_reference()
            .map_or_else(Vec::new, |reference| self.region_overlay_bounds(&reference));

        PaintData {
            pages,
            link_invert_overlays,
            diff_flash_overlays,
            highlight_overlays,
            selected_highlight_borders,
            note_underlines,
            link_highlights,
            linked_hover_highlights,
            failed_translation_highlights,
            selection_highlights,
            search_highlights,
            active_search_highlights,
            region_overlays,
        }
    }

    fn translation_hover_highlights(
        &mut self,
        reduce_motion: bool,
        window: &mut Window,
    ) -> Vec<TranslationHoverHighlight> {
        self.update_translation_hover_animation(reduce_motion, window)
            .into_iter()
            .collect()
    }

    pub(super) fn update_translation_hover_animation(
        &mut self,
        reduce_motion: bool,
        window: &mut Window,
    ) -> Option<TranslationHoverHighlight> {
        if !translation_pair_highlights_enabled(
            self.translation.view_role,
            self.translation.pair_active,
            self.translation.overlay_enabled,
        ) {
            self.translation.hover_animation = None;
            return None;
        }
        let region_id = match self.translation.view_role {
            TranslationViewRole::Translation => self.translation.hovered_region.as_ref(),
            TranslationViewRole::SourceMirror => self.translation.external_hovered_region.as_ref(),
            TranslationViewRole::None => None,
        }
        .cloned();
        let target = region_id.as_ref().and_then(|region_id| {
            self.translation_hover_region_highlights(region_id)
                .into_iter()
                .reduce(|left, right| left.union(&right))
        });
        let now = Instant::now();
        if let (Some(region_id), Some(target)) = (region_id, target) {
            let animation = self
                .translation
                .hover_animation
                .get_or_insert_with(|| TranslationHoverAnimation::new(region_id.clone(), target));
            animation.retarget(region_id, target, now, reduce_motion);
        } else if let Some(animation) = self.translation.hover_animation.as_mut() {
            // Keep the geometry after the fade completes. A later target can
            // then retarget the existing box instead of recreating it at the
            // destination and producing an instant disappear/reappear jump.
            animation.fade_out(now, reduce_motion);
        }

        let animation = self.translation.hover_animation.as_ref()?;
        if animation.is_animating(now) {
            window.request_animation_frame();
        }
        let opacity = animation.opacity.value(now).clamp(0.0, 1.0);
        if opacity <= 0.001 && !animation.opacity.is_animating(now) {
            return None;
        }
        Some(TranslationHoverHighlight {
            bounds: animation.bounds(now),
            opacity,
        })
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

        self.viewport_render_keys.clear();
        self.viewport_render_keys.insert(key);
        self.update_render_cache_protection(cx);

        let pdf_render_engine = cx
            .global::<SettingsStore>()
            .settings
            .document_view
            .pdf_render_engine;
        let native_pdf =
            self.format == DocumentFormat::Pdf && pdf_render_engine.uses_native_reader();
        let native_key = if native_pdf {
            self.native_gpu_render_key(page_idx, image_bounds, scale_factor)
        } else {
            key
        };
        let strict_native_pdf =
            self.format == DocumentFormat::Pdf && pdf_render_engine.is_strict_native();
        if strict_native_pdf {
            self.pending_renders.clear();
        }

        if !native_pdf {
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
        }

        self.viewport_render_debug.clear();
        let adjacent_gpu_page = self.prune_native_gpu_pages(&[page_idx], cx);
        let gpu_texture = self.native_gpu_texture(
            native_key,
            image_bounds,
            bounds,
            GpuSurfaceIntent::Visible,
            window,
            cx,
        );
        if let Some(adjacent_page) = adjacent_gpu_page {
            let adjacent_scale = self.presentation_scale_for_page(adjacent_page);
            let adjacent_bounds =
                self.presentation_image_bounds_for_page(adjacent_page, bounds, adjacent_scale);
            let adjacent_bounds = Self::pixel_aligned_image_bounds(adjacent_bounds, scale_factor);
            let adjacent_key =
                self.native_gpu_render_key(adjacent_page, adjacent_bounds, scale_factor);
            self.warm_native_gpu_surface(
                adjacent_key,
                adjacent_bounds,
                bounds,
                adjacent_page > page_idx,
                window,
                cx,
            );
        }
        let mupdf_allowed = u32::try_from(page_idx)
            .is_ok_and(|page| self.native_gpu_mupdf_placeholder_allowed(page, pdf_render_engine));
        let (image, displayed) = if gpu_texture.is_some() || !mupdf_allowed {
            (None, None)
        } else {
            self.cached_or_request_viewport_render(key, true, cx)
        };
        self.viewport_render_debug.insert(
            page_idx,
            ViewportPageRenderDebug {
                requested: native_key,
                displayed,
            },
        );
        self.assemble_paint_data(
            vec![PaintPage {
                page_idx,
                bounds: image_bounds,
                image,
                gpu_texture,
            }],
            cx.global::<SettingsStore>().settings.behavior.reduce_motion,
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
        let viewport_height = bounds.size.height.as_f32().max(1.0);
        let viewport_changed = (self.viewport_width - viewport_width).abs() > 0.5
            || (self.viewport_height - viewport_height).abs() > 0.5;
        let translation_viewport_was_ready = self.translation_viewport_geometry_ready();
        let resize_anchor = viewport_changed
            .then(|| self.live_translation_viewport_anchor(SynchronizedScrollMode::Immediate))
            .flatten();
        self.viewport_origin = bounds.origin;
        self.viewport_width = viewport_width;
        self.viewport_height = viewport_height;
        if viewport_changed {
            self.mark_native_gpu_motion(cx);
            self.translation_panel_viewport_changed();
        }
        self.schedule_reflow_for_viewport(viewport_width, cx);
        self.base_scale = if self.capabilities.reflow {
            1.0
        } else {
            viewport_width / self.max_page_width.max(1.0)
        };
        if let (Some(session_id), Some(anchor)) = (self.translation.pair_session_id, resize_anchor)
        {
            self.apply_live_translation_viewport_anchor(session_id, anchor, cx);
        }
        let pending_anchor = self.translation.pending_viewport_anchor;
        self.apply_pending_live_translation_viewport_anchor(cx);
        let resize_was_clamped = resize_anchor.is_some_and(|previous| {
            self.live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
                .is_some_and(|current| !translation_viewport_anchors_match(previous, current))
        });
        let pending_was_clamped = pending_anchor.is_some_and(|pending| {
            self.live_translation_viewport_anchor(pending.mode)
                .is_some_and(|current| !translation_viewport_anchors_match(pending, current))
        });
        if self.translation_viewport_geometry_ready()
            && ((!translation_viewport_was_ready
                && self.translation.view_role == TranslationViewRole::SourceMirror)
                || resize_was_clamped
                || pending_was_clamped)
        {
            self.emit_live_translation_viewport_changed(SynchronizedScrollMode::Immediate, cx);
        }
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

        let first_candidate = self.page_index_for_doc_y(self.scroll_y_doc);
        for page_idx in first_candidate..self.pages.len() {
            let (image_bounds, _) =
                self.image_bounds_and_render_key(page_idx, bounds, scale, scale_factor);
            let page_top = image_bounds.origin.y.as_f32();
            let page_bottom = page_top + image_bounds.size.height.as_f32();

            if page_top > viewport_bottom {
                break;
            }

            if page_bottom >= viewport_top && page_top <= viewport_bottom {
                self.page_image_bounds[page_idx] = Some(image_bounds);
                visible_pages.push(page_idx);
            }
        }

        let mut visible_page_renders = Vec::with_capacity(visible_pages.len());
        for &page_idx in &visible_pages {
            let (image_bounds, key) =
                self.image_bounds_and_render_key(page_idx, bounds, scale, scale_factor);
            visible_page_renders.push((page_idx, image_bounds, key));
        }
        self.viewport_render_keys.clear();
        self.viewport_render_keys
            .extend(visible_page_renders.iter().map(|(_, _, key)| *key));
        self.update_render_cache_protection(cx);

        let pdf_render_engine = cx
            .global::<SettingsStore>()
            .settings
            .document_view
            .pdf_render_engine;
        let native_pdf =
            self.format == DocumentFormat::Pdf && pdf_render_engine.uses_native_reader();
        let strict_native_pdf =
            self.format == DocumentFormat::Pdf && pdf_render_engine.is_strict_native();
        if strict_native_pdf {
            self.pending_renders.clear();
        }

        if !native_pdf
            && !self.dock_snapshot.layout_motion_active
            && let (Some((first, _, _)), Some((last, _, _))) =
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

        self.viewport_render_debug.clear();
        let adjacent_gpu_page = self.prune_native_gpu_pages(&visible_pages, cx);
        let adjacent_gpu_render = adjacent_gpu_page.map(|page_idx| {
            let (image_bounds, key) =
                self.image_bounds_and_render_key(page_idx, bounds, scale, scale_factor);
            let forward = visible_pages.last().is_none_or(|last| page_idx > *last);
            (key, image_bounds, forward)
        });
        let mut pages = Vec::with_capacity(visible_page_renders.len());
        let mut has_rendered_page = false;
        for (page_idx, image_bounds, key) in visible_page_renders {
            let native_key = if native_pdf {
                self.native_gpu_render_key(page_idx, image_bounds, scale_factor)
            } else {
                key
            };
            let gpu_texture = self.native_gpu_texture(
                native_key,
                image_bounds,
                bounds,
                GpuSurfaceIntent::Visible,
                window,
                cx,
            );
            let mupdf_allowed = u32::try_from(page_idx).is_ok_and(|page| {
                self.native_gpu_mupdf_placeholder_allowed(page, pdf_render_engine)
            });
            let (image, fallback_key) = if gpu_texture.is_some() || !mupdf_allowed {
                (None, None)
            } else {
                self.cached_or_request_viewport_render(
                    key,
                    !self.dock_snapshot.layout_motion_active,
                    cx,
                )
            };
            self.viewport_render_debug.insert(
                page_idx,
                ViewportPageRenderDebug {
                    requested: native_key,
                    displayed: fallback_key,
                },
            );
            if let Some(fallback_key) = fallback_key
                && fallback_key.doc_id == self.doc_id
            {
                self.viewport_render_keys.insert(fallback_key);
            }
            has_rendered_page |= image.is_some() || gpu_texture.is_some();
            pages.push(PaintPage {
                page_idx,
                bounds: image_bounds,
                image,
                gpu_texture,
            });
        }
        if let Some((key, image_bounds, forward)) = adjacent_gpu_render {
            let native_key = self.native_gpu_render_key(key.page_idx, image_bounds, scale_factor);
            self.warm_native_gpu_surface(native_key, image_bounds, bounds, forward, window, cx);
        }
        // Desired keys were protected before resolution so a final render can
        // land safely. Add the actual fallback keys as well so cache pressure
        // from thumbnails or adjacent pages cannot blank the animated view.
        self.update_render_cache_protection(cx);
        if has_rendered_page && !strict_native_pdf {
            self.start_text_indexing(cx);
        }

        Ok(self.assemble_paint_data(
            pages,
            cx.global::<SettingsStore>().settings.behavior.reduce_motion,
            window,
        ))
    }
}
