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
    }

    fn mention_preview_content(
        payload: MentionPreviewPayload,
        cx: &mut Context<Self>,
    ) -> MentionPreviewContent {
        match payload {
            MentionPreviewPayload::Text(text) => {
                let text = limit_mention_preview_text(text);
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
        hovered: Option<(Bounds<Pixels>, ComposerMention)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((trigger_bounds, mention)) = hovered else {
            self.schedule_mention_preview_hide(window, cx);
            return;
        };
        if matches!(
            mention.reference.kind,
            MentionKind::DocumentImage | MentionKind::SelectedPagesImage
        ) {
            self.schedule_mention_preview_hide(window, cx);
            return;
        }
        self.cancel_mention_preview_hide();

        let available_above = trigger_bounds.top();
        let available_below = window.viewport_size().height - trigger_bounds.bottom();
        let (preview_position, preview_anchor) = if available_below >= available_above {
            (
                point(trigger_bounds.left(), trigger_bounds.bottom()),
                Anchor::TopLeft,
            )
        } else {
            (
                point(trigger_bounds.left(), trigger_bounds.top()),
                Anchor::BottomLeft,
            )
        };
        if let Some(preview) = self.mention_preview.as_mut()
            && preview.reference == mention.reference
            && preview.source_range == mention.range
        {
            preview.trigger_bounds = trigger_bounds;
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
                reference: mention.reference,
                source_range: mention.range,
                trigger_bounds,
                position: preview_position,
                anchor: preview_anchor,
                generation,
                status: MentionPreviewStatus::Ready(Self::mention_preview_content(
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
            reference: mention.reference,
            source_range: mention.range.clone(),
            trigger_bounds,
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
                        mention_preview_matches(preview, generation, reference, &source_range)
                    }) {
                        let status = result
                            .map(|payload| {
                                MentionPreviewStatus::Ready(Self::mention_preview_content(
                                    payload, cx,
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
        let selected_pages = mention.captured_pages().cloned();
        let references = selected_pages.as_ref().map_or_else(
            || vec![reference],
            |pages| {
                pages
                    .pages
                    .iter()
                    .map(|page| MentionReference::new(reference.kind, Some(page.0 + 1)))
                    .collect()
            },
        );
        let resolve_task = self
            .context_provider
            .resolve_page_references(references, cx);
        self.pending_mention_preview = Some(cx.spawn(async move |this, cx| {
            let result = match resolve_task.await {
                Ok(resolved) => selected_pages.as_ref().map_or_else(
                    || preview_content_from_resolved(reference, &resolved),
                    |pages| preview_content_from_selected_pages(reference.kind, pages, &resolved),
                ),
                Err(error) => Err(format!("{error:#}")),
            };
            let _ = this.update(cx, |this, cx| {
                if this.mention_preview.as_ref().is_some_and(|preview| {
                    mention_preview_matches(preview, generation, reference, &source_range)
                }) {
                    let status = result
                        .map(|payload| {
                            MentionPreviewStatus::Ready(Self::mention_preview_content(payload, cx))
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
        let max_height = (available - MENTION_PREVIEW_BRIDGE_HEIGHT).max(0.0);
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
}

impl AiPanel {
    fn current_region_snapshot(&self, cx: &App) -> Option<RegionImageReference> {
        self.context_provider
            .context(false, cx)
            .and_then(|context| context.selected_region)
            .filter(|region| !region.is_empty())
    }

    fn current_selected_pages_snapshot(&self, cx: &App) -> Option<SelectedPagesReference> {
        self.context_provider
            .context(false, cx)
            .and_then(|context| context.selected_pages)
            .filter(|pages| !pages.is_empty())
    }

    pub(super) fn current_mention_availability(&self, cx: &App) -> (bool, bool, bool) {
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
            context
                .as_ref()
                .and_then(|context| context.selected_pages.as_ref())
                .is_some_and(|pages| !pages.is_empty()),
        )
    }

    pub(super) fn mention_is_available(&self, mention: MentionKind, cx: &App) -> bool {
        let (has_selection, has_region, has_selected_pages) = self.current_mention_availability(cx);
        mention_is_available(
            mention,
            self.page_count,
            has_selection,
            has_region,
            has_selected_pages,
        )
    }

    pub(super) fn apply_regular_edit(&mut self, range: Range<usize>, new_text: &str) {
        self.clear_validation_notice();
        self.content =
            (self.content[..range.start].to_owned() + new_text + &self.content[range.end..]).into();
        adjust_composer_mentions(&mut self.composer_mentions, &range, new_text.len());
        let cursor = range.start + new_text.len();
        self.selected_range = cursor..cursor;
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
        self.active_composer_input()
            .focus_handle(cx)
            .focus(window, cx);
        self.close_composer_menus();
        if mention.requires_fixed_layout() {
            self.update_current_page(cx);
        }
        if !self.mention_is_available(mention, cx) {
            cx.notify();
            return;
        }
        if !matches!(
            mention,
            MentionKind::Selection
                | MentionKind::RegionImage
                | MentionKind::SelectedPagesText
                | MentionKind::SelectedPagesImage
        ) && self
            .composer_mentions
            .iter()
            .any(|composer_mention| composer_mention.reference.kind == mention)
            && !mention.requires_page_number()
        {
            cx.notify();
            return;
        }

        let cursor = self.cursor_offset(cx).min(self.content.len());
        self.invalidate_mention_preview();
        let page = self.current_page.clamp(1, self.page_count.max(1));
        let reference =
            MentionReference::new(mention, mention.requires_page_number().then_some(page));
        self.insert_mention_at_range(reference, cursor..cursor, cx);
        self.sync_composer_input(window, cx);
    }

    fn capture_mention(
        &mut self,
        mention: MentionKind,
        cx: &mut Context<Self>,
    ) -> Option<Option<CapturedMention>> {
        if mention.requires_fixed_layout() {
            self.update_current_page(cx);
        }
        if !self.mention_is_available(mention, cx) {
            return None;
        }
        match mention {
            MentionKind::Selection => self
                .current_selection_snapshot(cx)
                .map(CapturedMention::Text)
                .map(Some),
            MentionKind::RegionImage => self
                .current_region_snapshot(cx)
                .map(CapturedMention::Region)
                .map(Some),
            MentionKind::SelectedPagesText | MentionKind::SelectedPagesImage => self
                .current_selected_pages_snapshot(cx)
                .map(CapturedMention::Pages)
                .map(Some),
            MentionKind::Page
            | MentionKind::PageImage
            | MentionKind::DocumentText
            | MentionKind::DocumentImage => Some(None),
        }
    }

    fn insert_mention_at_range(
        &mut self,
        reference: MentionReference,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) {
        let Some(captured) = self.capture_mention(reference.kind, cx) else {
            cx.notify();
            return;
        };
        let (new_text, relative_mention_range) =
            mention_insertion_text(&self.content, &range, &reference.display_text());
        let mention_start = range.start + relative_mention_range.start;
        let mention_end = range.start + relative_mention_range.end;
        let insertion_start = range.start;
        self.apply_regular_edit(range, &new_text);
        let token_id = self.new_mention_token_id();
        self.composer_mentions.push(ComposerMention {
            reference,
            range: mention_start..mention_end,
            token_id,
            captured,
        });
        self.composer_mentions
            .sort_by_key(|composer_mention| composer_mention.range.start);
        let cursor = insertion_start + new_text.len();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    fn new_mention_token_id(&mut self) -> SharedString {
        self.next_mention_token += 1;
        format!("mention:{}", self.next_mention_token).into()
    }

    /// Give every mention without a token ID (rebuilt from a sent message)
    /// a fresh one, so each composer token maps back to exactly one mention.
    pub(super) fn assign_mention_token_ids(&mut self) {
        for ix in 0..self.composer_mentions.len() {
            if self.composer_mentions[ix].token_id.is_empty() {
                let token_id = self.new_mention_token_id();
                self.composer_mentions[ix].token_id = token_id;
            }
        }
    }

    fn mention_token(mention: &ComposerMention) -> InlineToken {
        let label = match mention.reference.page {
            Some(page) if mention.reference.kind.requires_page_number() => {
                format!("{} {page}", mention.reference.kind.label())
            }
            _ => mention.reference.kind.label().to_string(),
        };
        InlineToken::new(mention.token_id.clone(), mention.reference.display_text())
            .with_label(label)
    }

    /// The composer text with one inline token per mention, as the input
    /// should show it.
    pub(super) fn composer_input_content(&self) -> InputContent {
        self.composer_mentions.iter().fold(
            InputContent::new(self.content.clone()),
            |content, mention| {
                content
                    .with_token(mention.range.clone(), Self::mention_token(mention))
                    .unwrap_or_else(|_| InputContent::new(self.content.clone()))
            },
        )
    }

    /// Re-read mention ranges from the input's tokens after the user edited
    /// the text. A mention whose token was deleted is dropped.
    pub(super) fn sync_mentions_from_input(&mut self, content: &InputContent) {
        self.composer_mentions.retain_mut(|mention| {
            let Some(span) = content
                .tokens()
                .iter()
                .find(|span| span.token().id() == &mention.token_id)
            else {
                return false;
            };
            mention.range = span.range();
            true
        });
        self.composer_mentions
            .sort_by_key(|composer_mention| composer_mention.range.start);
    }

    /// Show, update or hide the `@` menu for the caret position.
    pub(super) fn refresh_mention_menu(&mut self, cx: &App) {
        let cursor = self.selected_range.end.min(self.content.len());
        let Some(range) = (self.selected_range.is_empty())
            .then(|| active_mention_range(&self.content, cursor, &self.composer_mentions))
            .flatten()
        else {
            self.mention_menu = None;
            return;
        };
        let query = &self.content[range.start + '@'.len_utf8()..range.end];
        let (has_selection, has_region, has_selected_pages) = self.current_mention_availability(cx);
        let page_count = self.page_count;
        let candidates = mention_menu_candidates(query, self.current_page, page_count, |kind| {
            mention_is_available(
                kind,
                page_count,
                has_selection,
                has_region,
                has_selected_pages,
            )
        });
        if candidates.is_empty() {
            self.mention_menu = None;
            return;
        }
        let selected = self
            .mention_menu
            .as_ref()
            .filter(|menu| menu.range.start == range.start)
            .map_or(0, |menu| menu.selected.min(candidates.len() - 1));
        self.mention_menu = Some(MentionMenu {
            range,
            candidates,
            selected,
        });
    }

    pub(super) fn move_mention_menu_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(menu) = self.mention_menu.as_mut() {
            let len = menu.candidates.len() as isize;
            menu.selected = (menu.selected as isize + delta).rem_euclid(len) as usize;
            cx.notify();
        }
    }

    pub(super) fn accept_mention_menu(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.mention_menu.take() else {
            return;
        };
        let Some(reference) = menu.candidates.get(index.unwrap_or(menu.selected)).copied() else {
            return;
        };
        if reference.kind.requires_fixed_layout() {
            self.update_current_page(cx);
        }
        self.invalidate_mention_preview();
        self.insert_mention_at_range(reference, menu.range, cx);
        self.sync_composer_input(window, cx);
    }

    pub(super) fn mention_icon(kind: MentionKind) -> IconName {
        if kind.is_image() {
            IconName::Image
        } else {
            IconName::FileText
        }
    }
}
