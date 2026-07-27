use super::helpers::*;
use super::*;
use glyphst_core::{PageIndex, PageRegion};
use gpui::{TestAppContext, point, px};
use gpui_component::Root;

#[derive(Clone, Copy)]
struct EmptyAiContextProvider;

impl AiContextProvider for EmptyAiContextProvider {
    fn context(&self, _: bool, _: &App) -> Option<AiContext> {
        Some(AiContext::default())
    }

    fn page_state(&self, _: &App) -> Option<AiPageState> {
        Some(AiPageState::default())
    }

    fn resolve_page_references(
        &self,
        _: Vec<MentionReference>,
        _: &App,
    ) -> Task<Result<Vec<ResolvedPageContext>>> {
        Task::ready(Ok(Vec::new()))
    }

    fn resolve_region_references(
        &self,
        _: Vec<RegionImageReference>,
        _: &App,
    ) -> Task<Result<Vec<ResolvedRegionContext>>> {
        Task::ready(Ok(Vec::new()))
    }
}

struct MessageScrollTestView {
    handle: ScrollHandle,
    row_heights: Vec<Pixels>,
}

impl Render for MessageScrollTestView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .flex()
            .flex_col()
            .w(px(300.0))
            .h(px(200.0))
            .child(
                div()
                    .id("message-scroll-test")
                    .flex()
                    .flex_col()
                    .p_3()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.handle)
                    .children(
                        self.row_heights
                            .iter()
                            .map(|height| div().flex_none().h(*height)),
                    ),
            )
            .vertical_scrollbar_for(&self.handle, window, cx)
    }
}

fn install_test_settings(cx: &mut App) {
    gpui_component::init(cx);
    let settings = glyphst_settings::Settings::default();
    let appearance = glyphst_settings::theme::ThemeAppearance::Light;
    let theme_catalog =
        glyphst_settings::theme::ThemeCatalog::load(std::path::Path::new("/nonexistent"));
    let resolved_theme = theme_catalog.resolve(settings.theme.name(appearance), settings.overrides);
    cx.set_global(SettingsStore {
        settings,
        theme_catalog,
        resolved_theme,
        system_appearance: appearance,
        user_managed: false,
        config_path: PathBuf::new(),
        state_path: PathBuf::new(),
    });
}

fn message_scroll_metrics(content_height: Pixels) -> (GpuiPoint<Pixels>, GpuiPoint<Pixels>) {
    let mut cx = TestAppContext::single();
    cx.update(install_test_settings);
    let handle = ScrollHandle::new();
    let window = cx.add_window({
        let handle = handle.clone();
        move |_, _| MessageScrollTestView {
            handle,
            row_heights: vec![content_height],
        }
    });

    cx.update_window(window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })
    .unwrap();
    handle.scroll_to_bottom();
    cx.update_window(window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })
    .unwrap();

    (handle.offset(), handle.max_offset())
}

fn message_scroll_item_offset(row_heights: Vec<Pixels>, item_index: usize) -> Pixels {
    let mut cx = TestAppContext::single();
    cx.update(install_test_settings);
    let handle = ScrollHandle::new();
    let window = cx.add_window({
        let handle = handle.clone();
        move |_, _| MessageScrollTestView {
            handle,
            row_heights,
        }
    });

    cx.update_window(window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })
    .unwrap();
    handle.scroll_to_top_of_item(item_index);
    cx.update_window(window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })
    .unwrap();

    handle.offset().y
}

fn region_reference(page: usize, x0: f32) -> RegionImageReference {
    RegionImageReference::new([PageRegion {
        page: PageIndex(page),
        x0,
        y0: 2.0,
        x1: x0 + 10.0,
        y1: 12.0,
    }])
}

#[test]
fn request_prompt_strips_structured_mentions_and_preserves_words() {
    let text = "Compare @page(12) with @selection\nusing @document text.";
    let mentions = parse_composer_mentions(text);
    assert_eq!(
        request_prompt_without_mentions(text, &mentions),
        "Compare with\nusing."
    );
}

#[test]
fn failed_submit_retries_the_failed_assistant() {
    let failure = GenerationFailure {
        assistant_index: 3,
        message: "offline".to_string(),
    };

    assert_eq!(submit_action(false, Some(&failure)), SubmitAction::Retry(3));
    assert_eq!(submit_action(true, Some(&failure)), SubmitAction::Edit);
    assert_eq!(submit_action(false, None), SubmitAction::Send);
}

#[test]
fn loading_dot_opacity_cycles_between_dots() {
    let phases = [0.0, 1.0 / 3.0, 2.0 / 3.0];

    for (active, delta) in phases.into_iter().enumerate() {
        let opacities = phases.map(|phase| loading_dot_opacity(delta, phase));
        let active_opacity = opacities[active];
        assert!(active_opacity > 0.99);
        assert!(
            opacities
                .into_iter()
                .enumerate()
                .all(|(index, opacity)| index == active || opacity < active_opacity)
        );
    }
}

#[test]
fn message_scrollbar_host_does_not_create_false_overflow() {
    let (offset, max_offset) = message_scroll_metrics(px(40.0));
    assert_eq!(max_offset.y, px(0.0));
    assert_eq!(offset.y, px(0.0));
}

#[test]
fn message_scrollbar_host_preserves_real_overflow() {
    let (offset, max_offset) = message_scroll_metrics(px(400.0));
    assert!(max_offset.y > px(0.0));
    assert_eq!(offset.y, -max_offset.y);
}

#[test]
fn message_scrollbar_host_preserves_item_targeting() {
    assert_eq!(
        message_scroll_item_offset(vec![px(100.0); 4], 1),
        px(-112.0)
    );
}

#[gpui::test]
fn clicking_outside_an_edited_user_message_cancels_editing(cx: &mut TestAppContext) {
    cx.update(|cx| {
        install_test_settings(cx);
        cx.set_global(LibraryStore::memory());
    });

    let (root, cx) = cx.add_window_view(|window, cx| {
        let previous_focus_handle = cx.focus_handle();
        let panel = cx.new(|cx| {
            AiPanel::new(
                previous_focus_handle,
                EmptyAiContextProvider,
                PathBuf::from("/tmp/editing-test.pdf"),
                AiPageState {
                    page_count: 1,
                    current_page: 1,
                },
                AI_PANEL_WIDTH,
                window,
                cx,
            )
        });
        panel.update(cx, |panel, cx| {
            let message = ChatMessage::user_text("Edit me".to_string());
            panel
                .markdown_views
                .push(cx.new(|cx| Markdown::new(message.display_text().into(), cx)));
            panel.messages.push(message);
            panel.start_editing_message(0, window, cx);
        });
        Root::new(panel, window, cx)
    });
    let panel = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<AiPanel>().unwrap()
    });

    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    panel.read_with(cx, |panel, _| {
        assert!(panel.editing_turn.is_some());
    });

    cx.simulate_click(point(px(8.0), px(8.0)), gpui::Modifiers::default());

    panel.read_with(cx, |panel, _| {
        assert!(panel.editing_turn.is_none());
    });
}

#[test]
fn edit_adjusts_ranges_and_removes_overlapping_mentions() {
    let mut mentions = parse_composer_mentions("@selection then @page(3)");
    adjust_composer_mentions(&mut mentions, &(0..0), 4);
    assert_eq!(mentions[0].range, 4..14);
    assert_eq!(mentions[1].range, 20..28);

    adjust_composer_mentions(&mut mentions, &(6..8), 1);
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].range, 19..27);
}

#[test]
fn parser_restores_only_canonical_mentions_at_valid_boundaries() {
    let mentions = parse_composer_mentions(
        "mail@example.com @page(2) (@page-image(4)) @selection @document-image @region-image",
    );
    assert_eq!(
        mentions
            .into_iter()
            .map(|mention| mention.reference)
            .collect::<Vec<_>>(),
        vec![
            MentionReference::new(MentionKind::Page, Some(2)),
            MentionReference::new(MentionKind::PageImage, Some(4)),
            MentionReference::new(MentionKind::Selection, None),
            MentionReference::new(MentionKind::DocumentImage, None),
            MentionReference::new(MentionKind::RegionImage, None),
        ]
    );
}

#[test]
fn selection_mentions_keep_distinct_snapshots_in_order() {
    let mut existing = parse_composer_mentions("@selection");
    existing[0].captured = Some(CapturedMention::Text("First selection".to_string()));
    adjust_composer_mentions(&mut existing, &(0..0), "prefix ".len());

    let parsed = parse_composer_mentions("prefix @selection and @selection");
    let mentions = merge_composer_mentions(parsed, &existing, Some("Second selection".to_string()));
    let references = references_for_composer_mentions(&mentions);
    let live_context = AiContext {
        selected_text: Some("Later selection".to_string()),
        ..AiContext::default()
    };
    let contexts = context_blocks_for_composer_mentions(&mentions, Some(&live_context));

    assert_eq!(
        references,
        vec![
            MentionReference::new(MentionKind::Selection, None),
            MentionReference::new(MentionKind::Selection, None),
        ]
    );
    assert_eq!(
        contexts
            .iter()
            .map(|context| context.text.as_str())
            .collect::<Vec<_>>(),
        vec!["First selection", "Second selection"]
    );
}

#[test]
fn region_mentions_keep_distinct_geometry_snapshots() {
    let mut mentions = parse_composer_mentions("@region-image and @region-image");
    let first = region_reference(0, 1.0);
    let second = region_reference(2, 4.0);
    mentions[0].captured = Some(CapturedMention::Region(first.clone()));
    mentions[1].captured = Some(CapturedMention::Region(second.clone()));

    assert!(references_for_composer_mentions(&mentions).is_empty());
    assert_eq!(
        region_references_for_composer_mentions(&mentions),
        vec![first, second]
    );
}

#[test]
fn mention_insertion_uses_only_a_leading_source_separator() {
    assert_eq!(
        mention_insertion_text("Ask", &(3..3), "@selection"),
        (" @selection".to_string(), 1..11)
    );
    assert_eq!(
        mention_insertion_text("Ask  later", &(5..5), "@selection"),
        ("@selection".to_string(), 0..10)
    );
}

#[test]
fn deleting_selection_mention_removes_only_its_snapshot() {
    let mut mentions = parse_composer_mentions("@selection and @selection");
    mentions[0].captured = Some(CapturedMention::Text("First selection".to_string()));
    mentions[1].captured = Some(CapturedMention::Text("Second selection".to_string()));
    let first_range = mentions[0].range.clone();

    adjust_composer_mentions(&mut mentions, &first_range, 0);

    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].captured_text(), Some("Second selection"));
    assert_eq!(mentions[0].range.start, 5);
}

#[test]
fn non_selection_references_remain_deduplicated() {
    let mentions =
        parse_composer_mentions("@document @document @page(2) @page(2) @selection @selection");

    assert_eq!(
        references_for_composer_mentions(&mentions),
        vec![
            MentionReference::new(MentionKind::DocumentText, None),
            MentionReference::new(MentionKind::Page, Some(2)),
            MentionReference::new(MentionKind::Selection, None),
            MentionReference::new(MentionKind::Selection, None),
        ]
    );
}

#[test]
fn mention_trigger_requires_a_boundary() {
    assert_eq!(active_mention_range("ask @pa", 7, &[]), Some(4..7));
    assert_eq!(active_mention_range("@ ", 1, &[]), Some(0..1));
    assert_eq!(active_mention_range("mail@example", 12, &[]), None);
    assert_eq!(active_mention_range("(@page", 6, &[]), Some(1..6));
    assert_eq!(active_mention_range("ask @pa now", 11, &[]), None);
    assert_eq!(active_mention_range("ask @pa\n", 8, &[]), None);
}

#[test]
fn mention_trigger_accepts_an_explicit_mention_boundary() {
    let selection = parse_composer_mentions("@selection");
    assert_eq!(active_mention_range("@selection", 5, &selection), None);
    assert_eq!(active_mention_range("@selection", 10, &selection), None);
    assert_eq!(
        active_mention_range("@selection@", 11, &selection),
        Some(10..11)
    );
    assert_eq!(
        active_mention_range("@selection@pa", 13, &selection),
        Some(10..13)
    );
    assert_eq!(active_mention_range("@selection@", 11, &[]), None);

    let page = parse_composer_mentions("@page(12)");
    assert_eq!(active_mention_range("@page(12)@", 10, &page), Some(9..10));
    assert_eq!(active_mention_range("@selectionx@", 12, &selection), None);
}

#[test]
fn completion_uses_canonical_order_and_fuzzy_label_matching() {
    assert_eq!(
        mention_matches("")
            .into_iter()
            .map(|candidate| candidate.kind)
            .collect::<Vec<_>>(),
        MentionKind::ALL
    );
    assert_eq!(
        mention_matches("pi")
            .into_iter()
            .map(|candidate| candidate.kind)
            .collect::<Vec<_>>(),
        vec![MentionKind::PageImage]
    );
    assert_eq!(
        mention_matches("di")
            .into_iter()
            .map(|candidate| candidate.kind)
            .collect::<Vec<_>>(),
        vec![MentionKind::DocumentImage]
    );
}

#[test]
fn selection_mentions_are_available_only_with_selected_text() {
    assert!(!mention_is_available(
        MentionKind::Selection,
        10,
        false,
        false
    ));
    assert!(mention_is_available(MentionKind::Selection, 0, true, false));
    assert!(!mention_is_available(
        MentionKind::RegionImage,
        10,
        false,
        false
    ));
    assert!(mention_is_available(
        MentionKind::RegionImage,
        10,
        false,
        true
    ));
    assert!(!mention_is_available(MentionKind::Page, 0, true, true));
    assert!(mention_is_available(MentionKind::Page, 1, false, false));
    assert!(mention_is_available(
        MentionKind::DocumentText,
        0,
        false,
        false
    ));
}

#[test]
fn manual_mention_tokens_remain_plain_without_explicit_mentions() {
    let source = "Compare @selection with @document";

    assert!(references_for_composer_mentions(&[]).is_empty());
    assert_eq!(request_prompt_without_mentions(source, &[]), source);
}

#[test]
fn ordinary_edits_preserve_only_existing_explicit_mentions() {
    let mut mentions = parse_composer_mentions("@selection");
    mentions[0].captured = Some(CapturedMention::Text("Selected text".to_string()));

    adjust_composer_mentions(&mut mentions, &(0..0), "@document ".len());

    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].reference.kind, MentionKind::Selection);
    assert_eq!(mentions[0].range, 10..20);
    assert_eq!(mentions[0].captured_text(), Some("Selected text"));
}

#[test]
fn page_digit_edits_preserve_explicit_mention_metadata() {
    let mut mentions = parse_composer_mentions("@page(12)");

    adjust_composer_mentions_for_input_change(&mut mentions, &(6..8), 1, "@page(3)");

    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].range, 0..8);
    assert_eq!(
        mentions[0].reference,
        MentionReference::new(MentionKind::Page, Some(3))
    );
    assert_eq!(composer_page_digits_range(&mentions[0]), Some(6..7));
}

#[test]
fn empty_page_digit_edit_keeps_span_until_input_clamps_it() {
    let mut mentions = parse_composer_mentions("@page(12)");

    adjust_composer_mentions_for_input_change(&mut mentions, &(6..8), 0, "@page()");

    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].range, 0..7);
    assert_eq!(
        mentions[0].reference,
        MentionReference::new(MentionKind::Page, Some(12))
    );
    assert_eq!(composer_page_digits_range(&mentions[0]), Some(6..6));

    adjust_composer_mentions_for_input_change(&mut mentions, &(6..6), 1, "@page(1)");
    assert_eq!(
        mentions[0].reference,
        MentionReference::new(MentionKind::Page, Some(1))
    );
}

#[test]
fn edits_outside_page_digits_remove_the_explicit_mention() {
    let mut mentions = parse_composer_mentions("@page(12)");

    adjust_composer_mentions_for_input_change(&mut mentions, &(2..3), 1, "@pxge(12)");

    assert!(mentions.is_empty());
}

#[test]
fn trimmed_composer_content_rebases_explicit_mentions() {
    let source = "  Ask @selection  ";
    let mut mentions = parse_composer_mentions(source);
    mentions[0].captured = Some(CapturedMention::Text("Selected text".to_string()));

    let (trimmed, trimmed_mentions) = trimmed_composer_content(source, &mentions);

    assert_eq!(trimmed, "Ask @selection");
    assert_eq!(trimmed_mentions[0].range, 4..14);
    assert_eq!(trimmed_mentions[0].captured_text(), Some("Selected text"));
}

#[test]
fn all_text_previews_are_limited_at_a_unicode_boundary() {
    let source = "文".repeat(MENTION_PREVIEW_MAX_CHARS + 1);
    let limited = limit_mention_preview_text(source);

    assert!(limited.truncated);
    assert_eq!(limited.text.chars().count(), MENTION_PREVIEW_MAX_CHARS);

    let exact_limit = "文".repeat(MENTION_PREVIEW_MAX_CHARS);
    assert_eq!(
        limit_mention_preview_text(exact_limit.clone()),
        LimitedMentionPreviewText {
            text: exact_limit,
            truncated: false,
        }
    );
}

#[test]
fn async_preview_results_require_matching_identity() {
    let reference = MentionReference::new(MentionKind::Page, Some(2));
    let preview = MentionPreview {
        reference,
        source_range: 4..12,
        trigger_bounds: Bounds {
            origin: point(px(10.0), px(10.0)),
            size: gpui::size(px(80.0), px(20.0)),
        },
        position: point(px(0.0), px(0.0)),
        anchor: Anchor::TopLeft,
        generation: 7,
        status: MentionPreviewStatus::Loading,
    };

    assert!(mention_preview_matches(&preview, 7, reference, &(4..12)));
    assert!(!mention_preview_matches(&preview, 8, reference, &(4..12)));
    assert!(!mention_preview_matches(
        &preview,
        7,
        MentionReference::new(MentionKind::Page, Some(3)),
        &(4..12)
    ));
    assert!(!mention_preview_matches(&preview, 7, reference, &(5..13)));
}

#[test]
fn preview_hover_region_includes_trigger_and_panel() {
    let trigger_bounds = Bounds {
        origin: point(px(10.0), px(10.0)),
        size: gpui::size(px(80.0), px(20.0)),
    };
    let preview_bounds = Bounds {
        origin: point(px(10.0), px(30.0)),
        size: gpui::size(px(320.0), px(126.0)),
    };

    assert!(mention_preview_region_contains(
        point(px(20.0), px(20.0)),
        Some(trigger_bounds),
        Some(preview_bounds),
    ));
    assert!(mention_preview_region_contains(
        point(px(20.0), px(33.0)),
        Some(trigger_bounds),
        Some(preview_bounds),
    ));
    assert!(!mention_preview_region_contains(
        point(px(340.0), px(160.0)),
        Some(trigger_bounds),
        Some(preview_bounds),
    ));
}

#[test]
fn mention_preview_sizes_clamp_and_preserve_image_aspect_ratio() {
    assert_eq!(AiPanel::mention_preview_width(400.0, 800.0), 320.0);
    assert_eq!(AiPanel::mention_preview_width(240.0, 800.0), 240.0);
    assert_eq!(AiPanel::mention_preview_width(400.0, 300.0), 284.0);
    assert_eq!(AiPanel::mention_preview_width(0.0, 10.0), 0.0);
    assert_eq!(
        AiPanel::mention_preview_image_size(320.0, 640.0, 1_000, 2_000),
        (320.0, 640.0)
    );
    assert_eq!(
        AiPanel::mention_preview_image_size(320.0, 640.0, 2_000, 1_000),
        (320.0, 160.0)
    );
    assert_eq!(
        AiPanel::mention_preview_image_size(320.0, 220.0, 1_000, 2_000),
        (110.0, 220.0)
    );
}

#[test]
fn page_resolution_skips_hydrated_references() {
    let reference = MentionReference::new(MentionKind::PageImage, Some(2));
    let mut hydrated = ChatMessage::user(
        "@page-image(2)".to_string(),
        String::new(),
        Vec::new(),
        vec![reference],
    );
    hydrated.attach_image(glyphst_ai::ai_chat::ImageContext::new(
        MentionKind::PageImage,
        Some(2),
        "image/jpeg",
        vec![1, 2, 3],
    ));
    assert!(unresolved_page_references(&[hydrated], 2).is_empty());
}

#[test]
fn hydration_preserves_existing_page_contexts() {
    let first = MentionReference::new(MentionKind::Page, Some(1));
    let second = MentionReference::new(MentionKind::Page, Some(2));
    let mut message = ChatMessage::user(
        "@page(1) @page(2)".to_string(),
        String::new(),
        Vec::new(),
        vec![first, second],
    );
    message.attach_context(ContextBlock::new(
        MentionKind::Page,
        Some(1),
        "First".to_string(),
    ));

    hydrate_page_contexts(
        std::slice::from_mut(&mut message),
        &[ResolvedPageContext::Text(ContextBlock::new(
            MentionKind::Page,
            Some(2),
            "Second".to_string(),
        ))],
    );

    assert!(message.has_resolved_page_reference(first));
    assert!(message.has_resolved_page_reference(second));
    let request = message.request_text();
    assert!(request.contains("First"));
    assert!(request.contains("Second"));
}

#[test]
fn image_preflight_counts_full_history_and_document_pages() {
    let history = ChatMessage::user(
        "@page-image(2)".to_string(),
        String::new(),
        Vec::new(),
        vec![MentionReference::new(MentionKind::PageImage, Some(2))],
    );
    let prospective = ChatMessage::user(
        "@document-image".to_string(),
        String::new(),
        Vec::new(),
        vec![MentionReference::new(MentionKind::DocumentImage, None)],
    );
    let messages = vec![history, prospective];

    assert!(validate_page_image_preflight(&messages, 3, 3).is_err());
    assert!(validate_page_image_preflight(&messages, 3, 4).is_ok());
    assert!(validate_page_image_preflight(&messages[1..], 0, 10).is_err());
}

#[test]
fn image_preflight_counts_region_images() {
    let reference = RegionImageReference::new([
        region_reference(0, 1.0).regions[0],
        region_reference(1, 2.0).regions[0],
    ]);
    let message = ChatMessage::user(
        "@region-image".to_string(),
        String::new(),
        Vec::new(),
        Vec::new(),
    )
    .with_region_references(vec![reference]);

    assert!(validate_page_image_preflight([&message], 2, 1).is_err());
    assert!(validate_page_image_preflight([&message], 2, 2).is_ok());
}

#[test]
fn quick_ask_preflight_excludes_the_replaced_exchange() {
    let history = ChatMessage::user(
        "@page-image(1)".to_string(),
        String::new(),
        Vec::new(),
        vec![MentionReference::new(MentionKind::PageImage, Some(1))],
    );
    let pending = ChatMessage::user(
        "@page-image(2)".to_string(),
        String::new(),
        Vec::new(),
        vec![MentionReference::new(MentionKind::PageImage, Some(2))],
    );

    assert!(
        validate_composer_page_image_preflight(
            AiPanelMode::QuickAsk,
            std::slice::from_ref(&history),
            &pending,
            2,
            1,
        )
        .is_ok()
    );
    assert!(
        validate_composer_page_image_preflight(
            AiPanelMode::Sidebar,
            std::slice::from_ref(&history),
            &pending,
            2,
            1,
        )
        .is_err()
    );
}

#[test]
fn document_image_hydration_attaches_every_resolved_page() {
    let document_image = MentionReference::new(MentionKind::DocumentImage, None);
    let mut message = ChatMessage::user(
        "@document-image".to_string(),
        String::new(),
        Vec::new(),
        vec![document_image],
    );
    hydrate_page_contexts(
        std::slice::from_mut(&mut message),
        &[
            ResolvedPageContext::Image(glyphst_ai::ai_chat::ImageContext::new(
                MentionKind::DocumentImage,
                Some(1),
                "image/jpeg",
                vec![1],
            )),
            ResolvedPageContext::Image(glyphst_ai::ai_chat::ImageContext::new(
                MentionKind::DocumentImage,
                Some(2),
                "image/jpeg",
                vec![2],
            )),
        ],
    );

    assert!(message_has_resolved_reference(&message, document_image, 2));
    assert!(unresolved_page_references(&[message], 2).is_empty());
}

#[test]
fn document_image_persistence_mapping_round_trips() {
    let reference = MentionReference::new(MentionKind::DocumentImage, None);
    assert_eq!(
        library_reference_to_ai(&ai_reference_to_library(reference)),
        Some(reference)
    );
}

#[test]
fn region_hydration_and_persistence_keep_geometry() {
    let reference = region_reference(1, 3.0);
    let mut message = ChatMessage::user(
        "@region-image".to_string(),
        String::new(),
        Vec::new(),
        Vec::new(),
    )
    .with_region_references(vec![reference.clone()]);
    assert_eq!(
        unresolved_region_references(std::slice::from_ref(&message)),
        vec![reference.clone()]
    );

    hydrate_region_contexts(
        std::slice::from_mut(&mut message),
        &[ResolvedRegionContext {
            reference: reference.clone(),
            image: glyphst_ai::ai_chat::ImageContext::new_region(
                reference.clone(),
                "image/jpeg",
                vec![1, 2, 3],
            ),
        }],
    );
    assert!(unresolved_region_references(&[message]).is_empty());

    let stored = AiMentionReference::RegionImage {
        reference: reference.clone(),
    };
    assert_eq!(library_reference_to_region(&stored), Some(reference));
    assert_eq!(library_reference_to_ai(&stored), None);
}

#[test]
fn ambiguous_persisted_mention_occurrences_are_not_editable() {
    let message = ChatMessage::user(
        "@selection and literal @selection".to_string(),
        "and literal @selection".to_string(),
        vec![ContextBlock::new(
            MentionKind::Selection,
            None,
            "original selection".to_string(),
        )],
        vec![MentionReference::new(MentionKind::Selection, None)],
    );

    assert!(composer_mentions_for_message(&message).is_err());
}

#[test]
fn live_mention_occurrences_restore_the_exact_attached_token() {
    let text = "literal @selection and attached @selection";
    let ranges = find_mention_references(text);
    let attached = ranges[1].clone();
    let message = ChatMessage::user(
        text.to_string(),
        "literal @selection and attached".to_string(),
        vec![ContextBlock::new(
            MentionKind::Selection,
            None,
            "original selection".to_string(),
        )],
        vec![MentionReference::new(MentionKind::Selection, None)],
    )
    .with_mention_occurrences(vec![MentionOccurrence::new(attached.0.clone(), attached.1)]);

    let mentions = composer_mentions_for_message(&message).unwrap();
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].range, attached.0);
    assert_eq!(mentions[0].captured_text(), Some("original selection"));
}

#[test]
fn duplicate_document_occurrences_share_the_historical_snapshot() {
    let text = "@document and @document";
    let occurrences = find_mention_references(text)
        .into_iter()
        .map(|(range, reference)| MentionOccurrence::new(range, reference))
        .collect();
    let message = ChatMessage::user(
        text.to_string(),
        "and".to_string(),
        vec![ContextBlock::new(
            MentionKind::DocumentText,
            None,
            "historical document".to_string(),
        )],
        vec![MentionReference::new(MentionKind::DocumentText, None)],
    )
    .with_mention_occurrences(occurrences);

    let mentions = composer_mentions_for_message(&message).unwrap();
    assert_eq!(mentions.len(), 2);
    assert!(
        mentions
            .iter()
            .all(|mention| mention.captured_text() == Some("historical document"))
    );
}

#[test]
fn persisted_occurrences_preserve_region_geometry() {
    let reference = region_reference(2, 4.0);
    let message = ChatMessage::user("@region".to_string(), String::new(), Vec::new(), Vec::new())
        .with_region_references(vec![reference.clone()])
        .with_mention_occurrences(vec![
            MentionOccurrence::new(0..7, MentionReference::new(MentionKind::RegionImage, None))
                .with_region_reference(reference.clone()),
        ]);

    let persisted = persisted_session_messages(&[message], None);
    assert_eq!(persisted[0].occurrences.len(), 1);
    assert_eq!(
        library_occurrence_to_ai(&persisted[0].occurrences[0])
            .unwrap()
            .region_reference,
        Some(reference)
    );
}

#[test]
fn persisted_selection_context_is_restored_for_editing() {
    let original = ChatMessage::user(
        "Explain @selection".to_string(),
        "Explain".to_string(),
        vec![ContextBlock::new(
            MentionKind::Selection,
            None,
            "historical text".to_string(),
        )],
        vec![MentionReference::new(MentionKind::Selection, None)],
    );
    let persisted = ChatMessage::from_persisted(
        Role::User,
        original.display_text(),
        Some(original.request_text()),
    )
    .with_references(original.references().to_vec());

    let mentions = composer_mentions_for_message(&persisted).unwrap();
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].captured_text(), Some("historical text"));
}

#[test]
fn failed_assistant_message_is_excluded_from_persistence() {
    let messages = vec![
        ChatMessage::user(
            "Question".to_string(),
            "Question".to_string(),
            Vec::new(),
            Vec::new(),
        ),
        ChatMessage::assistant("Partial response".to_string()),
    ];

    let persisted = persisted_session_messages(&messages, Some(1));
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].role, AiSessionRole::User);
    assert_eq!(persisted[0].content, "Question");
}

#[test]
fn captured_document_context_wins_during_regeneration() {
    let mentions = [ComposerMention {
        reference: MentionReference::new(MentionKind::DocumentText, None),
        range: 0..9,
        captured: Some(CapturedMention::Text("historical document".to_string())),
    }];
    let current = AiContext {
        document_text: Some("new document".to_string()),
        ..Default::default()
    };

    let contexts = context_blocks_for_composer_mentions(&mentions, Some(&current));
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].text, "historical document");
}

#[test]
fn message_scroll_distance_uses_negative_scroll_offsets() {
    assert_eq!(message_scroll_distance_from_bottom(0.0, 300.0), 300.0);
    assert_eq!(message_scroll_distance_from_bottom(-175.0, 300.0), 125.0);
    assert_eq!(message_scroll_distance_from_bottom(-300.0, 300.0), 0.0);
    assert_eq!(message_scroll_distance_from_bottom(-350.0, 300.0), 0.0);
}
