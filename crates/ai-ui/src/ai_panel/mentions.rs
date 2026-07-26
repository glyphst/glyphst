use super::helpers::*;
use super::*;

impl AiPanel {
    pub(super) fn cancel_mention_preview_hide(&mut self) {
        self.mention_preview_hide_task = None;
    }

    fn pointer_in_mention_preview_region(&self, window: &Window) -> bool {
        mention_preview_region_contains(
            window.mouse_position(),
            self.mention_preview
                .as_ref()
                .map(|preview| preview.trigger_bounds),
            self.mention_preview_bounds.get(),
        )
    }

    pub(super) fn schedule_mention_preview_hide(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pointer_in_mention_preview_region(window) {
            self.cancel_mention_preview_hide();
            return;
        }
        if self.mention_preview.is_none() || self.mention_preview_hide_task.is_some() {
            return;
        }
        let generation = self.mention_preview_generation;
        self.mention_preview_hide_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(MENTION_PREVIEW_HIDE_DELAY)
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.mention_preview_generation == generation
                    && this.mention_preview_hide_task.is_some()
                {
                    if this.pointer_in_mention_preview_region(window) {
                        this.cancel_mention_preview_hide();
                    } else {
                        this.hide_mention_preview(cx);
                    }
                }
            });
        }));
    }

    pub(super) fn hide_mention_preview(&mut self, cx: &mut Context<Self>) {
        if self.mention_preview.is_none()
            && self.pending_mention_preview.is_none()
            && self.mention_preview_hide_task.is_none()
        {
            return;
        }
        self.mention_preview_generation = self.mention_preview_generation.wrapping_add(1);
        self.mention_preview = None;
        self.pending_mention_preview = None;
        self.mention_preview_hide_task = None;
        self.mention_preview_bounds.set(None);
        cx.notify();
    }

    pub(super) fn invalidate_mention_preview(&mut self) {
        self.mention_preview_generation = self.mention_preview_generation.wrapping_add(1);
        self.mention_preview = None;
        self.pending_mention_preview = None;
        self.mention_preview_hide_task = None;
        self.mention_preview_bounds.set(None);
        self.mention_hitboxes.clear();
    }

    fn mention_preview_content(
        kind: MentionKind,
        payload: MentionPreviewPayload,
        cx: &mut Context<Self>,
    ) -> MentionPreviewContent {
        match payload {
            MentionPreviewPayload::Text(text) => {
                let text = limit_mention_preview_text(kind, text);
                MentionPreviewContent::Text {
                    markdown: cx.new(|cx| Markdown::new(text.text.into(), cx)),
                    truncated: text.truncated,
                }
            }
            MentionPreviewPayload::Image(image) => MentionPreviewContent::Image(image),
        }
    }

    pub(super) fn update_mention_preview(
        &mut self,
        source: MentionPreviewSource,
        hovered: Option<(MentionHitbox, ComposerMention)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((hitbox, mention)) = hovered else {
            self.schedule_mention_preview_hide(window, cx);
            return;
        };
        if mention.reference.kind == MentionKind::DocumentImage {
            self.schedule_mention_preview_hide(window, cx);
            return;
        }
        self.cancel_mention_preview_hide();

        let available_above = hitbox.bounds.top();
        let available_below = window.viewport_size().height - hitbox.bounds.bottom();
        let (preview_position, preview_anchor) = if available_below >= available_above {
            (
                point(hitbox.bounds.left(), hitbox.bounds.bottom()),
                Anchor::TopLeft,
            )
        } else {
            (
                point(hitbox.bounds.left(), hitbox.bounds.top()),
                Anchor::BottomLeft,
            )
        };
        if let Some(preview) = self.mention_preview.as_mut()
            && preview.source == source
            && preview.reference == mention.reference
            && preview.source_range == mention.range
        {
            preview.trigger_bounds = hitbox.bounds;
            if preview.position != preview_position || preview.anchor != preview_anchor {
                preview.position = preview_position;
                preview.anchor = preview_anchor;
                cx.notify();
            }
            return;
        }

        self.mention_preview_generation = self.mention_preview_generation.wrapping_add(1);
        let generation = self.mention_preview_generation;
        self.pending_mention_preview = None;
        self.mention_preview_bounds.set(None);

        let context = if mention.captured_text().is_some() {
            None
        } else {
            let include_document_text = mention.reference.kind == MentionKind::DocumentText;
            matches!(
                mention.reference.kind,
                MentionKind::Selection | MentionKind::DocumentText
            )
            .then(|| self.context_provider.context(include_document_text, cx))
            .flatten()
        };
        if let Some(text) = mention_preview_text(&mention, context.as_ref()) {
            self.mention_preview = Some(MentionPreview {
                source,
                reference: mention.reference,
                source_range: mention.range,
                trigger_bounds: hitbox.bounds,
                position: preview_position,
                anchor: preview_anchor,
                generation,
                status: MentionPreviewStatus::Ready(Self::mention_preview_content(
                    mention.reference.kind,
                    MentionPreviewPayload::Text(text),
                    cx,
                )),
            });
            self.mention_preview_scroll
                .set_offset(point(px(0.0), px(0.0)));
            cx.notify();
            return;
        }

        self.mention_preview = Some(MentionPreview {
            source,
            reference: mention.reference,
            source_range: mention.range.clone(),
            trigger_bounds: hitbox.bounds,
            position: preview_position,
            anchor: preview_anchor,
            generation,
            status: MentionPreviewStatus::Loading,
        });
        self.mention_preview_scroll
            .set_offset(point(px(0.0), px(0.0)));
        cx.notify();

        let reference = mention.reference;
        let source_range = mention.range.clone();
        if reference.kind == MentionKind::RegionImage {
            let Some(region) = mention.captured_region().cloned() else {
                self.mention_preview = None;
                cx.notify();
                return;
            };
            let resolve_task = self
                .context_provider
                .resolve_region_references(vec![region.clone()], cx);
            self.pending_mention_preview = Some(cx.spawn(async move |this, cx| {
                let result = match resolve_task.await {
                    Ok(resolved) => preview_content_from_resolved_region(&region, &resolved),
                    Err(error) => Err(format!("{error:#}")),
                };
                let _ = this.update(cx, |this, cx| {
                    if this.mention_preview.as_ref().is_some_and(|preview| {
                        mention_preview_matches(
                            preview,
                            source,
                            generation,
                            reference,
                            &source_range,
                        )
                    }) {
                        let status = result
                            .map(|payload| {
                                MentionPreviewStatus::Ready(Self::mention_preview_content(
                                    reference.kind,
                                    payload,
                                    cx,
                                ))
                            })
                            .unwrap_or_else(MentionPreviewStatus::Error);
                        if let Some(preview) = this.mention_preview.as_mut() {
                            preview.status = status;
                        }
                        this.pending_mention_preview = None;
                        cx.notify();
                    }
                });
            }));
            return;
        }
        let resolve_task = self
            .context_provider
            .resolve_page_references(vec![reference], cx);
        self.pending_mention_preview = Some(cx.spawn(async move |this, cx| {
            let result = match resolve_task.await {
                Ok(resolved) => preview_content_from_resolved(reference, &resolved),
                Err(error) => Err(format!("{error:#}")),
            };
            let _ = this.update(cx, |this, cx| {
                if this.mention_preview.as_ref().is_some_and(|preview| {
                    mention_preview_matches(preview, source, generation, reference, &source_range)
                }) {
                    let status = result
                        .map(|payload| {
                            MentionPreviewStatus::Ready(Self::mention_preview_content(
                                reference.kind,
                                payload,
                                cx,
                            ))
                        })
                        .unwrap_or_else(MentionPreviewStatus::Error);
                    if let Some(preview) = this.mention_preview.as_mut() {
                        preview.status = status;
                    }
                    this.pending_mention_preview = None;
                    cx.notify();
                }
            });
        }));
    }
    fn current_region_snapshot(&self, cx: &App) -> Option<RegionImageReference> {
        self.context_provider
            .context(false, cx)
            .and_then(|context| context.selected_region)
            .filter(|region| !region.is_empty())
    }

    pub(super) fn current_mention_availability(&self, cx: &App) -> (bool, bool) {
        let context = self.context_provider.context(false, cx);
        (
            context
                .as_ref()
                .and_then(|context| context.selected_text.as_deref())
                .is_some_and(|text| !text.trim().is_empty()),
            context
                .as_ref()
                .and_then(|context| context.selected_region.as_ref())
                .is_some_and(|region| !region.is_empty()),
        )
    }

    pub(super) fn mention_is_available(&self, mention: MentionKind, cx: &App) -> bool {
        let (has_selection, has_region) = self.current_mention_availability(cx);
        mention_is_available(mention, self.page_count, has_selection, has_region)
    }

    pub(super) fn mention_at_offset(&self, offset: usize) -> Option<&ComposerMention> {
        self.composer_mentions
            .iter()
            .find(|mention| mention.range.start <= offset && offset <= mention.range.end)
    }

    pub(super) fn page_digits_range(mention: &ComposerMention) -> Option<Range<usize>> {
        if !mention.reference.kind.requires_page_number() {
            return None;
        }
        let start = mention.range.start + mention.reference.kind.token().len() + 1;
        Some(start..mention.range.end.saturating_sub(1))
    }

    pub(super) fn begin_page_number_edit(
        &mut self,
        mention_start: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mention) = self
            .composer_mentions
            .iter()
            .find(|mention| mention.range.start == mention_start)
        else {
            return false;
        };
        let Some(digits) = Self::page_digits_range(mention) else {
            return false;
        };
        self.selected_range = digits.clone();
        self.selection_reversed = false;
        self.active_page_number_edit = Some(ActivePageNumberEdit {
            mention_start,
            digits,
        });
        self.composer_history.break_group();
        self.mention_completion = None;
        cx.notify();
        true
    }

    pub(super) fn finish_page_number_edit(&mut self, cx: &mut Context<Self>) -> bool {
        let before = self.composer_snapshot();
        let Some(active) = self.active_page_number_edit.take() else {
            return false;
        };
        self.clear_validation_notice();
        let page = validate_page_number(&self.content[active.digits.clone()], self.page_count);
        let page_text = page.to_string();
        let old_digits = active.digits.clone();
        self.invalidate_mention_preview();
        self.content = (self.content[..old_digits.start].to_owned()
            + &page_text
            + &self.content[old_digits.end..])
            .into();
        let new_digits = old_digits.start..old_digits.start + page_text.len();
        let delta = page_text.len() as isize - old_digits.len() as isize;
        adjust_mentions_for_page_number_edit(
            &mut self.composer_mentions,
            active.mention_start,
            new_digits.clone(),
            delta,
            page,
        );
        let cursor = self
            .composer_mentions
            .iter()
            .find(|mention| mention.range.start == active.mention_start)
            .map_or(new_digits.end + 1, |mention| mention.range.end);
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        self.last_display = None;
        self.last_lines.clear();
        self.finish_composer_edit(before, EditGroup::Isolated, cx);
        true
    }

    pub(super) fn apply_regular_edit(&mut self, range: Range<usize>, new_text: &str) {
        self.clear_validation_notice();
        self.invalidate_mention_preview();
        self.content =
            (self.content[..range.start].to_owned() + new_text + &self.content[range.end..]).into();
        adjust_composer_mentions(&mut self.composer_mentions, &range, new_text.len());
        self.active_page_number_edit = None;
        self.last_display = None;
        self.last_lines.clear();
        let cursor = range.start + new_text.len();
        self.selected_range = cursor..cursor;
    }

    pub(super) fn apply_page_number_edit(&mut self, range: Range<usize>, new_text: &str) -> bool {
        let Some(active) = self.active_page_number_edit.clone() else {
            return false;
        };
        if range.start < active.digits.start || range.end > active.digits.end {
            return false;
        }
        let filtered = new_text
            .bytes()
            .filter(u8::is_ascii_digit)
            .map(char::from)
            .collect::<String>();
        if !new_text.is_empty() && filtered.is_empty() {
            return true;
        }
        self.clear_validation_notice();
        self.invalidate_mention_preview();
        self.content =
            (self.content[..range.start].to_owned() + &filtered + &self.content[range.end..])
                .into();
        let delta = filtered.len() as isize - range.len() as isize;
        let new_digits = active.digits.start
            ..((active.digits.end as isize + delta).max(active.digits.start as isize) as usize);
        adjust_mentions_for_page_number_edit(
            &mut self.composer_mentions,
            active.mention_start,
            new_digits.clone(),
            delta,
            0,
        );
        self.active_page_number_edit = Some(ActivePageNumberEdit {
            mention_start: active.mention_start,
            digits: new_digits,
        });
        self.last_display = None;
        self.last_lines.clear();
        let cursor = range.start + filtered.len();
        self.selected_range = cursor..cursor;
        true
    }
    pub(super) fn update_mention_completion(&mut self, cx: &App) {
        let Some(source_range) = self.active_mention_source_range() else {
            self.mention_completion = None;
            return;
        };
        let matches = self.mention_matches_for_range(&source_range, cx);
        if matches.is_empty() {
            self.mention_completion = None;
            return;
        }

        let selected_ix = self
            .mention_completion
            .as_ref()
            .map(|completion| completion.selected_ix)
            .unwrap_or(0)
            .min(matches.len().saturating_sub(1));
        self.mention_completion = Some(MentionCompletion {
            source_range,
            selected_ix,
        });
        self.mention_completion_scroll
            .scroll_to_item(selected_ix, ScrollStrategy::Nearest);
    }

    fn active_mention_source_range(&self) -> Option<Range<usize>> {
        if !self.selected_range.is_empty() {
            return None;
        }

        let cursor = self.cursor_offset().min(self.content.len());
        active_mention_range(&self.content, cursor, &self.composer_mentions)
    }

    fn mention_matches_for_range(
        &self,
        source_range: &Range<usize>,
        cx: &App,
    ) -> Vec<MentionMatch> {
        if source_range.start >= source_range.end || source_range.end > self.content.len() {
            return Vec::new();
        }
        let query_start = source_range.start + '@'.len_utf8();
        if query_start > source_range.end {
            return Vec::new();
        }
        let query = &self.content[query_start..source_range.end];
        let (has_selection, has_region) = self.current_mention_availability(cx);
        mention_matches(query)
            .into_iter()
            .filter(|candidate| {
                mention_is_available(candidate.kind, self.page_count, has_selection, has_region)
            })
            .collect()
    }

    pub(super) fn mention_completion_matches(&self, cx: &App) -> Vec<MentionMatch> {
        self.mention_completion
            .as_ref()
            .map(|completion| self.mention_matches_for_range(&completion.source_range, cx))
            .unwrap_or_default()
    }

    pub(super) fn confirm_mention_completion(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(completion) = self.mention_completion.clone() else {
            return false;
        };
        let matches = self.mention_matches_for_range(&completion.source_range, cx);
        let Some(mention) = matches.get(completion.selected_ix) else {
            return false;
        };
        self.insert_mention_at_range(mention.kind, completion.source_range, cx);
        true
    }

    pub(super) fn select_previous_mention_completion(&mut self, cx: &mut Context<Self>) -> bool {
        let match_count = self.mention_completion_matches(cx).len();
        let Some(completion) = self.mention_completion.as_mut() else {
            return false;
        };
        if match_count == 0 {
            return false;
        }
        completion.selected_ix = if completion.selected_ix == 0 {
            match_count - 1
        } else {
            completion.selected_ix - 1
        };
        self.mention_completion_scroll
            .scroll_to_item(completion.selected_ix, ScrollStrategy::Nearest);
        cx.notify();
        true
    }

    pub(super) fn select_next_mention_completion(&mut self, cx: &mut Context<Self>) -> bool {
        let match_count = self.mention_completion_matches(cx).len();
        let Some(completion) = self.mention_completion.as_mut() else {
            return false;
        };
        if match_count == 0 {
            return false;
        }
        completion.selected_ix = (completion.selected_ix + 1) % match_count;
        self.mention_completion_scroll
            .scroll_to_item(completion.selected_ix, ScrollStrategy::Nearest);
        cx.notify();
        true
    }

    fn insert_mention(&mut self, mention: MentionKind, cx: &mut Context<Self>) {
        let range = self
            .mention_completion
            .as_ref()
            .map(|completion| completion.source_range.clone())
            .unwrap_or_else(|| {
                let cursor = self.cursor_offset().min(self.content.len());
                cursor..cursor
            });
        self.insert_mention_at_range(mention, range, cx);
    }

    pub(super) fn add_attachment(
        &mut self,
        mention: MentionKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading {
            return;
        }
        self.focus_handle.focus(window, cx);
        self.close_composer_menus();
        if mention.requires_fixed_layout() {
            self.update_current_page(cx);
        }
        if !self.mention_is_available(mention, cx) {
            cx.notify();
            return;
        }
        if !matches!(mention, MentionKind::Selection | MentionKind::RegionImage)
            && self
                .composer_mentions
                .iter()
                .any(|composer_mention| composer_mention.reference.kind == mention)
            && !mention.requires_page_number()
        {
            cx.notify();
            return;
        }

        let cursor = self.cursor_offset().min(self.content.len());
        self.insert_mention_at_range(mention, cursor..cursor, cx);
    }

    fn insert_mention_at_range(
        &mut self,
        mention: MentionKind,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) {
        let before = self.composer_snapshot();
        if mention.requires_fixed_layout() {
            self.update_current_page(cx);
        }
        if !self.mention_is_available(mention, cx) {
            self.mention_completion = None;
            cx.notify();
            return;
        }
        let captured = match mention {
            MentionKind::Selection => {
                let Some(selection) = self.current_selection_snapshot(cx) else {
                    self.mention_completion = None;
                    cx.notify();
                    return;
                };
                Some(CapturedMention::Text(selection))
            }
            MentionKind::RegionImage => {
                let Some(region) = self.current_region_snapshot(cx) else {
                    self.mention_completion = None;
                    cx.notify();
                    return;
                };
                Some(CapturedMention::Region(region))
            }
            MentionKind::Page
            | MentionKind::PageImage
            | MentionKind::DocumentText
            | MentionKind::DocumentImage => None,
        };
        let page = self.current_page.clamp(1, self.page_count.max(1));
        let token = if mention.requires_page_number() {
            MentionReference::new(mention, Some(page)).display_text()
        } else {
            mention.token().to_string()
        };
        let (new_text, relative_mention_range) =
            mention_insertion_text(&self.content, &range, &token);
        let mention_start = range.start + relative_mention_range.start;
        let mention_end = range.start + relative_mention_range.end;
        let insertion_start = range.start;
        self.apply_regular_edit(range, &new_text);
        let reference =
            MentionReference::new(mention, mention.requires_page_number().then_some(page));
        self.composer_mentions.push(ComposerMention {
            reference,
            range: mention_start..mention_end,
            captured,
        });
        self.composer_mentions
            .sort_by_key(|composer_mention| composer_mention.range.start);
        if mention.requires_page_number() {
            self.begin_page_number_edit(mention_start, cx);
        } else {
            let cursor = insertion_start + new_text.len();
            self.selected_range = cursor..cursor;
        }
        self.selection_reversed = false;
        self.marked_range = None;
        self.mention_completion = None;
        self.finish_composer_edit(before, EditGroup::Isolated, cx);
    }
    pub(super) fn mention_icon(kind: MentionKind) -> IconName {
        if kind.is_image() {
            IconName::Image
        } else {
            IconName::FileText
        }
    }

    fn completion_menu_width(matches: &[MentionMatch], available: f32) -> f32 {
        let estimated = matches
            .iter()
            .map(|candidate| {
                candidate.kind.label().chars().count()
                    + candidate.kind.description().chars().count()
                    + 9
            })
            .max()
            .unwrap_or(0) as f32
            * 6.5;
        estimated
            .clamp(COMPLETION_MENU_MIN_WIDTH, COMPLETION_MENU_MAX_WIDTH)
            .min(available.max(0.0))
    }

    pub(super) fn mention_preview_width(panel_width: f32, viewport_width: f32) -> f32 {
        MENTION_PREVIEW_WIDTH
            .min(panel_width.max(0.0))
            .min((viewport_width - MENTION_PREVIEW_WINDOW_MARGIN * 2.0).max(0.0))
    }

    pub(super) fn mention_preview_size(
        preview: Option<&MentionPreview>,
        panel_width: f32,
        viewport_width: f32,
        viewport_height: f32,
    ) -> (f32, f32) {
        let max_width = Self::mention_preview_width(panel_width, viewport_width);
        let Some(MentionPreview {
            position,
            anchor,
            status: MentionPreviewStatus::Ready(MentionPreviewContent::Image(image)),
            ..
        }) = preview
        else {
            return (max_width, 0.0);
        };
        let available = match anchor {
            Anchor::TopLeft => {
                viewport_height - position.y.as_f32() - MENTION_PREVIEW_WINDOW_MARGIN
            }
            Anchor::BottomLeft => position.y.as_f32() - MENTION_PREVIEW_WINDOW_MARGIN,
            _ => viewport_height - MENTION_PREVIEW_WINDOW_MARGIN * 2.0,
        };
        let max_height =
            (available - MENTION_PREVIEW_HEADER_HEIGHT - MENTION_PREVIEW_BRIDGE_HEIGHT).max(0.0);
        Self::mention_preview_image_size(max_width, max_height, image.width, image.height)
    }

    pub(super) fn mention_preview_image_size(
        max_width: f32,
        max_height: f32,
        image_width: u32,
        image_height: u32,
    ) -> (f32, f32) {
        if max_width <= 0.0 || max_height <= 0.0 || image_width == 0 || image_height == 0 {
            return (0.0, 0.0);
        }
        let height = max_width * image_height as f32 / image_width as f32;
        if height <= max_height {
            (max_width, height)
        } else {
            (max_width * max_height / height, max_height)
        }
    }

    pub(super) fn completion_menu_placement(
        &self,
        matches: &[MentionMatch],
        window: &Window,
    ) -> Option<CompletionMenuPlacement> {
        let (bounds, cursor) = self.last_bounds.zip(self.last_cursor_pos)?;
        let window_bounds = window.bounds();
        let available_width = (window_bounds.right() - bounds.left() - px(8.0)).as_f32();
        let width = Self::completion_menu_width(matches, available_width);
        let max_left = (window_bounds.right() - px(width) - px(8.0)).max(window_bounds.left());
        let left = (bounds.left() + cursor.x)
            .min(max_left)
            .max(window_bounds.left() + px(8.0));
        let menu_height = px(COMPLETION_MENU_ROW_HEIGHT
            * matches.len().min(COMPLETION_MENU_VISIBLE_ROWS) as f32
            + 8.0);
        let below = bounds.top() + cursor.y + window.line_height();
        let fits_below = below + menu_height + px(8.0) <= window_bounds.bottom();
        Some(if fits_below {
            CompletionMenuPlacement {
                position: point(left, below),
                anchor: Anchor::TopLeft,
                width,
            }
        } else {
            CompletionMenuPlacement {
                position: point(left, bounds.top() + cursor.y),
                anchor: Anchor::BottomLeft,
                width,
            }
        })
    }

    pub(super) fn completion_menu_bounds(
        &self,
        matches: &[MentionMatch],
        window: &Window,
    ) -> Option<Bounds<Pixels>> {
        if matches.is_empty() {
            return None;
        }
        let placement = self.completion_menu_placement(matches, window)?;
        let height = px(COMPLETION_MENU_ROW_HEIGHT
            * matches.len().min(COMPLETION_MENU_VISIBLE_ROWS) as f32
            + 8.0);
        let top = match placement.anchor {
            Anchor::TopLeft => placement.position.y,
            Anchor::BottomLeft => placement.position.y - height,
            _ => return None,
        };
        Some(Bounds::new(
            point(placement.position.x, top),
            size(px(placement.width), height),
        ))
    }

    pub(super) fn render_mention_completion_menu(
        matches: Vec<MentionMatch>,
        selected_ix: usize,
        width: f32,
        scroll_handle: UniformListScrollHandle,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let match_count = matches.len();
        let list_height =
            COMPLETION_MENU_ROW_HEIGHT * match_count.min(COMPLETION_MENU_VISIBLE_ROWS) as f32;
        let matches = Arc::new(matches);
        div()
            .occlude()
            .w(px(width))
            .flex()
            .flex_col()
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .shadow_sm()
            .overflow_hidden()
            .text_size(ui_rems(12.0))
            .child(
                uniform_list(
                    "ai-mention-completions",
                    match_count,
                    cx.processor(move |_this, visible_range: Range<usize>, _, cx| {
                        visible_range
                            .filter_map(|ix| {
                                let candidate = matches.get(ix)?.clone();
                                let selected = ix == selected_ix;
                                let label = candidate.kind.label();
                                let highlights = candidate
                                    .highlights
                                    .iter()
                                    .cloned()
                                    .map(|range| {
                                        (
                                            range,
                                            HighlightStyle {
                                                color: Some(rgb(theme.match_highlight).into()),
                                                font_weight: Some(FontWeight::BOLD),
                                                ..Default::default()
                                            },
                                        )
                                    })
                                    .collect::<Vec<_>>();
                                Some(
                                    div().h(px(COMPLETION_MENU_ROW_HEIGHT)).px_1().child(
                                        div()
                                            .id(("ai-mention-completion", ix))
                                            .size_full()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .px_2()
                                            .rounded(px(4.0))
                                            .bg(if selected {
                                                rgb(theme.element_selected)
                                            } else {
                                                rgba(0x00000000)
                                            })
                                            .cursor_pointer()
                                            .hover(move |style| style.bg(rgb(theme.element_hover)))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    this.insert_mention(candidate.kind, cx);
                                                    cx.stop_propagation();
                                                }),
                                            )
                                            .child(
                                                Icon::new(Self::mention_icon(candidate.kind))
                                                    .size(ui_rems(14.0))
                                                    .text_color(rgb(if selected {
                                                        theme.accent
                                                    } else {
                                                        theme.text_muted
                                                    })),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_col()
                                                    .min_w_0()
                                                    .gap(px(1.0))
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .text_color(rgb(if selected {
                                                                theme.accent
                                                            } else {
                                                                theme.text
                                                            }))
                                                            .child(
                                                                StyledText::new(label)
                                                                    .with_highlights(highlights),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .text_size(ui_rems(10.0))
                                                            .text_color(rgb(theme.text_muted))
                                                            .child(candidate.kind.description()),
                                                    ),
                                            ),
                                    ),
                                )
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .h(px(list_height))
                .track_scroll(&scroll_handle),
            )
    }
}
