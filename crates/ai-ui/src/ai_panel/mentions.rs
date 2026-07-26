use super::helpers::*;
use super::*;

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
            self.triggered.set(false);
            return empty_completions();
        };
        let panel = panel.read(cx);
        let content = text.to_string();
        let Some(source_range) = active_mention_range(
            &content,
            offset.min(content.len()),
            &panel.composer_mentions,
        ) else {
            self.triggered.set(false);
            return empty_completions();
        };
        let query_start = source_range.start + '@'.len_utf8();
        if query_start > source_range.end {
            self.triggered.set(false);
            return empty_completions();
        }

        let query = &content[query_start..source_range.end];
        let (has_selection, has_region) = panel.current_mention_availability(cx);
        let page_count = panel.page_count;
        let page = panel.current_page.clamp(1, page_count.max(1));
        let candidates = mention_matches(query)
            .into_iter()
            .filter(|candidate| {
                mention_is_available(candidate.kind, page_count, has_selection, has_region)
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

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut Context<InputState>) -> bool {
        if new_text.contains('@') {
            self.triggered.set(true);
        }
        self.triggered.get()
    }
}

impl AiPanel {
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

        let cursor = self.cursor_offset(cx).min(self.content.len());
        self.insert_mention_at_range(mention, cursor..cursor, cx);
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
    ) {
        let Some(captured) = self.capture_mention(mention, cx) else {
            cx.notify();
            return;
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
    }

    pub(super) fn mention_icon(kind: MentionKind) -> IconName {
        if kind.is_image() {
            IconName::Image
        } else {
            IconName::FileText
        }
    }

    pub(super) fn record_accepted_mention(
        &mut self,
        item: &CompletionItem,
        inserted_range: &Range<usize>,
        cx: &mut Context<Self>,
    ) {
        let Some(data) = item.data.as_ref().and_then(serde_json::Value::as_object) else {
            return;
        };
        let Some(kind) = data
            .get(MENTION_KIND_DATA_KEY)
            .and_then(serde_json::Value::as_str)
            .and_then(mention_kind_from_token)
        else {
            return;
        };
        let Some(relative_start) = data
            .get(MENTION_START_DATA_KEY)
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        else {
            return;
        };
        let Some(relative_end) = data
            .get(MENTION_END_DATA_KEY)
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        else {
            return;
        };
        let Some(start) = inserted_range.start.checked_add(relative_start) else {
            return;
        };
        let Some(end) = inserted_range.start.checked_add(relative_end) else {
            return;
        };
        if start > end || end > inserted_range.end || end > self.content.len() {
            return;
        }
        let Some(captured) = self.capture_mention(kind, cx) else {
            return;
        };
        let page = data
            .get(MENTION_PAGE_DATA_KEY)
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok());
        if kind.requires_page_number() && page.is_none() {
            return;
        }

        self.composer_mentions.push(ComposerMention {
            reference: MentionReference::new(kind, page),
            range: start..end,
            captured,
        });
        self.composer_mentions
            .sort_by_key(|composer_mention| composer_mention.range.start);
        cx.notify();
    }
}
