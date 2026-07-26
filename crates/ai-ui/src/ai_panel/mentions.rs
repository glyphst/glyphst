use super::helpers::*;
use super::*;

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
    pub(super) fn update_mention_completion(&mut self, cx: &App) {
        let Some(source_range) = self.active_mention_source_range(cx) else {
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

    fn active_mention_source_range(&self, cx: &App) -> Option<Range<usize>> {
        if !self.selected_range.is_empty() {
            return None;
        }

        let cursor = self.cursor_offset(cx).min(self.content.len());
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

    fn insert_mention(&mut self, mention: MentionKind, cx: &mut Context<Self>) {
        let range = self
            .mention_completion
            .as_ref()
            .map(|completion| completion.source_range.clone())
            .unwrap_or_else(|| {
                let cursor = self.cursor_offset(cx).min(self.content.len());
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

        let cursor = self.cursor_offset(cx).min(self.content.len());
        self.insert_mention_at_range(mention, cursor..cursor, cx);
    }

    fn insert_mention_at_range(
        &mut self,
        mention: MentionKind,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) {
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
        let cursor = insertion_start + new_text.len();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        self.mention_completion = None;
        self.update_mention_completion(cx);
        cx.notify();
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

    pub(super) fn completion_menu_placement(
        &self,
        matches: &[MentionMatch],
        window: &Window,
    ) -> Option<CompletionMenuPlacement> {
        let window_bounds = window.bounds();
        let panel_left = (window_bounds.right() - px(self.width)).max(window_bounds.left());
        let available_width = (window_bounds.right() - panel_left - px(16.0)).as_f32();
        let width = Self::completion_menu_width(matches, available_width);
        let max_left = (window_bounds.right() - px(width) - px(8.0)).max(window_bounds.left());
        let left = (panel_left + px(8.0))
            .min(max_left)
            .max(window_bounds.left() + px(8.0));
        Some(CompletionMenuPlacement {
            position: point(left, window_bounds.bottom() - px(8.0)),
            anchor: Anchor::BottomLeft,
            width,
        })
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
