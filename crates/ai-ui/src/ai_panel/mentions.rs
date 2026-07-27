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

const MENTION_KIND_DATA_KEY: &str = "glyphst_mention_kind";
const MENTION_START_DATA_KEY: &str = "glyphst_mention_start";
const MENTION_END_DATA_KEY: &str = "glyphst_mention_end";
const MENTION_PAGE_DATA_KEY: &str = "glyphst_mention_page";

fn mention_kind_from_token(token: &str) -> Option<MentionKind> {
    MentionKind::ALL
        .into_iter()
        .find(|kind| kind.token() == token)
}

fn empty_completions() -> Task<Result<CompletionResponse>> {
    Task::ready(Ok(CompletionResponse::Array(Vec::new())))
}

impl CompletionProvider for MentionCompletionProvider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        cx: &mut Context<InputState>,
    ) -> Task<Result<CompletionResponse>> {
        let Some(panel) = self.panel.upgrade() else {
            return empty_completions();
        };
        let panel = panel.read(cx);
        let content = text.to_string();
        let Some(source_range) = active_mention_range(
            &content,
            offset.min(content.len()),
            &panel.composer_mentions,
        ) else {
            return empty_completions();
        };
        let query_start = source_range.start + '@'.len_utf8();
        if query_start > source_range.end {
            return empty_completions();
        }

        let query = &content[query_start..source_range.end];
        let (has_selection, has_region, has_selected_pages) =
            panel.current_mention_availability(cx);
        let page_count = panel.page_count;
        let page = panel.current_page.clamp(1, page_count.max(1));
        let candidates = mention_matches(query)
            .into_iter()
            .filter(|candidate| {
                mention_is_available(
                    candidate.kind,
                    page_count,
                    has_selection,
                    has_region,
                    has_selected_pages,
                )
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return empty_completions();
        }

        let edit_range = LspRange {
            start: text.offset_to_position(source_range.start),
            end: text.offset_to_position(source_range.end),
        };
        let items = candidates
            .into_iter()
            .map(|candidate| {
                let kind = candidate.kind;
                let token = if kind.requires_page_number() {
                    MentionReference::new(kind, Some(page)).display_text()
                } else {
                    kind.token().to_string()
                };
                let (new_text, mention_range) =
                    mention_insertion_text(&content, &source_range, &token);
                CompletionItem {
                    label: kind.token().to_string(),
                    detail: Some(kind.description()),
                    filter_text: Some(kind.token().to_string()),
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                        range: edit_range.clone(),
                        new_text,
                    })),
                    data: Some(serde_json::json!({
                        "glyphst_mention_kind": kind.token(),
                        "glyphst_mention_start": mention_range.start,
                        "glyphst_mention_end": mention_range.end,
                        "glyphst_mention_page": kind.requires_page_number().then_some(page),
                    })),
                    ..Default::default()
                }
            })
            .collect();
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn completion_trigger_range(
        &self,
        text: &Rope,
        cursor: usize,
        _: CompletionTriggerEvent<'_>,
        cx: &mut Context<InputState>,
    ) -> Option<Range<usize>> {
        let panel = self.panel.upgrade()?;
        let panel = panel.read(cx);
        active_mention_range(
            &text.to_string(),
            cursor.min(text.len()),
            &panel.composer_mentions,
        )
    }

    fn completion_menu_options(&self) -> CompletionMenuOptions {
        CompletionMenuOptions {
            min_width: px(MENTION_COMPLETION_MIN_WIDTH),
            max_width: px(MENTION_COMPLETION_MAX_WIDTH),
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
        let inserted_span = self.insert_mention_at_range(mention, cursor..cursor, cx);
        self.sync_composer_input(window, cx);
        if let Some(span_range) = inserted_span {
            self.active_composer_input().update(cx, |input, cx| {
                input.activate_span_edit(span_range, window, cx);
            });
        }
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
        mention: MentionKind,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let Some(captured) = self.capture_mention(mention, cx) else {
            cx.notify();
            return None;
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
        let cursor = insertion_start + new_text.len();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
        mention
            .requires_page_number()
            .then_some(mention_start..mention_end)
    }

    pub(super) fn mention_icon(kind: MentionKind) -> IconName {
        if kind.is_image() {
            IconName::Image
        } else {
            IconName::FileText
        }
    }

    pub(super) fn composer_input_spans(&self) -> Vec<InputSpan> {
        self.composer_mentions
            .iter()
            .map(|mention| {
                let span = InputSpan::new(mention.range.clone(), mention.reference.kind.label())
                    .icon(Self::mention_icon(mention.reference.kind));
                if let Some(digits) = composer_page_digits_range(mention) {
                    span.editable_unsigned_integer(digits, 1, self.page_count.max(1))
                } else {
                    span
                }
            })
            .collect()
    }

    pub(super) fn record_accepted_mention(
        &mut self,
        item: &CompletionItem,
        inserted_range: &Range<usize>,
        cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let Some(data) = item.data.as_ref().and_then(serde_json::Value::as_object) else {
            return None;
        };
        let Some(kind) = data
            .get(MENTION_KIND_DATA_KEY)
            .and_then(serde_json::Value::as_str)
            .and_then(mention_kind_from_token)
        else {
            return None;
        };
        let Some(relative_start) = data
            .get(MENTION_START_DATA_KEY)
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        else {
            return None;
        };
        let Some(relative_end) = data
            .get(MENTION_END_DATA_KEY)
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        else {
            return None;
        };
        let Some(start) = inserted_range.start.checked_add(relative_start) else {
            return None;
        };
        let Some(end) = inserted_range.start.checked_add(relative_end) else {
            return None;
        };
        if start > end || end > inserted_range.end || end > self.content.len() {
            return None;
        }
        let Some(captured) = self.capture_mention(kind, cx) else {
            return None;
        };
        let page = data
            .get(MENTION_PAGE_DATA_KEY)
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok());
        if kind.requires_page_number() && page.is_none() {
            return None;
        }

        self.composer_mentions.push(ComposerMention {
            reference: MentionReference::new(kind, page),
            range: start..end,
            captured,
        });
        self.composer_mentions
            .sort_by_key(|composer_mention| composer_mention.range.start);
        cx.notify();
        kind.requires_page_number().then_some(start..end)
    }
}
