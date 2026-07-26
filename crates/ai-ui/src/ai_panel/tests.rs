use super::helpers::*;
use super::*;
use glyphst_core::{PageIndex, PageRegion};
use gpui::TestAppContext;

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
fn historical_message_editing_uses_one_line_auto_height() {
    let edited = AiInputSource::EditedMessage;
    assert_eq!(
        input_line_constraints(AiPanelMode::Sidebar, &edited, false),
        (false, 1, None)
    );

    let composer = AiInputSource::Composer;
    assert_eq!(
        input_line_constraints(AiPanelMode::Sidebar, &composer, false),
        (false, 3, Some(8))
    );
    assert_eq!(
        input_line_constraints(AiPanelMode::QuickAsk, &composer, false),
        (false, 1, Some(4))
    );
}

#[test]
fn measured_input_height_respects_composer_limits_without_clipping_messages() {
    let line_height = px(20.0);

    assert_eq!(
        constrained_input_height(px(180.0), line_height, 1, None),
        px(180.0)
    );
    assert_eq!(
        constrained_input_height(px(20.0), line_height, 3, Some(8)),
        px(60.0)
    );
    assert_eq!(
        constrained_input_height(px(200.0), line_height, 3, Some(8)),
        px(160.0)
    );
}

#[test]
fn measured_input_height_tracks_available_wrap_width() {
    let mut cx = TestAppContext::single();
    cx.update(install_test_settings);
    let handle = ScrollHandle::new();
    let window = cx.add_window({
        let handle = handle.clone();
        move |_, _| MessageScrollTestView {
            handle,
            row_heights: Vec::new(),
        }
    });
    let (narrow_height, wide_height, line_height) = cx
        .update_window(window.into(), |_, window, _| {
            let text: SharedString = "WWWWWWWWWWWWWWWWWWWW WWWWWWWWWWWWWWWWWWWW".into();
            let text_style = window.text_style();
            let font_size = text_style.font_size.to_pixels(window.rem_size());
            let line_height = window.line_height();
            let runs = [text_style.to_run(text.len())];
            let measure = |width| {
                window
                    .text_system()
                    .shape_text(text.clone(), font_size, &runs, Some(width), None)
                    .unwrap()
                    .iter()
                    .map(|line| line.size(line_height).height)
                    .sum::<Pixels>()
            };
            (measure(px(120.0)), measure(px(480.0)), line_height)
        })
        .unwrap();

    assert!(narrow_height > wide_height);
    assert_eq!(
        constrained_input_height(narrow_height, line_height, 1, None),
        narrow_height
    );
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
fn selection_preview_uses_the_hovered_occurrence_snapshot() {
    let mut mentions = parse_composer_mentions("@selection and @selection");
    mentions[0].captured = Some(CapturedMention::Text("First selection".to_string()));
    mentions[1].captured = Some(CapturedMention::Text("Second selection".to_string()));
    let live_context = AiContext {
        selected_text: Some("Later selection".to_string()),
        ..AiContext::default()
    };

    assert_eq!(
        mention_preview_text(&mentions[0], Some(&live_context)).as_deref(),
        Some("First selection")
    );
    assert_eq!(
        mention_preview_text(&mentions[1], Some(&live_context)).as_deref(),
        Some("Second selection")
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
fn document_text_preview_uses_current_document_context() {
    let mention = parse_composer_mentions("@document").remove(0);
    let context = AiContext {
        document_text: Some("Document body".to_string()),
        ..AiContext::default()
    };

    assert_eq!(
        mention_preview_text(&mention, Some(&context)).as_deref(),
        Some("Document body")
    );
}

#[test]
fn document_text_preview_prefers_the_historical_snapshot() {
    let mut mention = parse_composer_mentions("@document").remove(0);
    mention.captured = Some(CapturedMention::Text("Historical document".to_string()));
    let context = AiContext {
        document_text: Some("Current document".to_string()),
        ..AiContext::default()
    };

    assert_eq!(
        mention_preview_text(&mention, Some(&context)).as_deref(),
        Some("Historical document")
    );
}

#[test]
fn document_image_preview_is_disabled() {
    let mention = parse_composer_mentions("@document-image").remove(0);
    assert!(mention_preview_text(&mention, None).is_none());
}

#[test]
fn resolved_page_preview_matches_text_and_image_references() {
    let text_reference = MentionReference::new(MentionKind::Page, Some(2));
    let text = ResolvedPageContext::Text(ContextBlock::new(
        MentionKind::Page,
        Some(2),
        "Page text".to_string(),
    ));
    assert!(matches!(
        preview_content_from_resolved(text_reference, &[text]),
        Ok(MentionPreviewPayload::Text(text)) if text == "Page text"
    ));

    let image_reference = MentionReference::new(MentionKind::PageImage, Some(3));
    let mut image_bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut image_bytes)
        .encode(&[255; 4 * 6 * 3], 4, 6, image::ExtendedColorType::Rgb8)
        .unwrap();
    let image = ResolvedPageContext::Image(glyphst_ai::ai_chat::ImageContext::new(
        MentionKind::PageImage,
        Some(3),
        "image/jpeg",
        image_bytes.clone(),
    ));
    match preview_content_from_resolved(image_reference, &[image]) {
        Ok(MentionPreviewPayload::Image(image)) => {
            assert_eq!(image.image.format, ImageFormat::Jpeg);
            assert_eq!(image.image.bytes, image_bytes);
            assert_eq!((image.width, image.height), (4, 6));
        }
        Ok(MentionPreviewPayload::Text(_)) | Err(_) => panic!("expected image preview"),
    }
}

#[test]
fn resolved_region_preview_matches_captured_geometry() {
    let reference = region_reference(1, 3.0);
    let mut image_bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut image_bytes)
        .encode(&[255; 5 * 7 * 3], 5, 7, image::ExtendedColorType::Rgb8)
        .unwrap();
    let resolved = ResolvedRegionContext {
        reference: reference.clone(),
        image: glyphst_ai::ai_chat::ImageContext::new_region(
            reference.clone(),
            "image/jpeg",
            image_bytes,
        ),
    };

    assert!(matches!(
        preview_content_from_resolved_region(&reference, &[resolved]),
        Ok(MentionPreviewPayload::Image(image)) if (image.width, image.height) == (5, 7)
    ));
}

#[test]
fn document_preview_is_limited_at_a_unicode_boundary() {
    let source = "文".repeat(MENTION_PREVIEW_MAX_CHARS + 1);
    let limited = limit_mention_preview_text(MentionKind::DocumentText, source.clone());

    assert!(limited.truncated);
    assert_eq!(limited.text.chars().count(), MENTION_PREVIEW_MAX_CHARS);
    assert_eq!(source.chars().count(), MENTION_PREVIEW_MAX_CHARS + 1);

    assert_eq!(
        limit_mention_preview_text(MentionKind::Page, source.clone()),
        LimitedMentionPreviewText {
            text: source,
            truncated: false,
        }
    );
}

#[test]
fn async_preview_results_require_matching_identity() {
    let reference = MentionReference::new(MentionKind::Page, Some(2));
    let source = MentionPreviewSource::Message(3);
    let trigger_bounds = Bounds {
        origin: point(px(10.0), px(10.0)),
        size: size(px(80.0), px(20.0)),
    };
    let preview = MentionPreview {
        source,
        reference,
        source_range: 4..12,
        trigger_bounds,
        position: point(px(0.0), px(0.0)),
        anchor: Anchor::TopLeft,
        generation: 7,
        status: MentionPreviewStatus::Loading,
    };

    assert!(mention_preview_matches(
        &preview,
        source,
        7,
        reference,
        &(4..12)
    ));
    assert!(!mention_preview_matches(
        &preview,
        MentionPreviewSource::Message(4),
        7,
        reference,
        &(4..12)
    ));
    assert!(!mention_preview_matches(
        &preview,
        source,
        8,
        reference,
        &(4..12)
    ));
    assert!(!mention_preview_matches(
        &preview,
        source,
        7,
        MentionReference::new(MentionKind::Page, Some(3)),
        &(4..12)
    ));
    assert!(!mention_preview_matches(
        &preview,
        source,
        7,
        reference,
        &(5..13)
    ));
}

#[test]
fn preview_hover_region_includes_trigger_and_panel() {
    let trigger_bounds = Bounds {
        origin: point(px(10.0), px(10.0)),
        size: size(px(80.0), px(20.0)),
    };
    let preview_bounds = Bounds {
        origin: point(px(10.0), px(30.0)),
        size: size(px(320.0), px(126.0)),
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
fn mention_preview_width_clamps_to_panel_and_window() {
    assert_eq!(AiPanel::mention_preview_width(400.0, 800.0), 320.0);
    assert_eq!(AiPanel::mention_preview_width(240.0, 800.0), 240.0);
    assert_eq!(AiPanel::mention_preview_width(400.0, 300.0), 284.0);
    assert_eq!(AiPanel::mention_preview_width(0.0, 10.0), 0.0);
}

#[test]
fn mention_preview_image_size_preserves_aspect_ratio() {
    assert_eq!(
        AiPanel::mention_preview_image_size(320.0, 640.0, 1_000, 2_000),
        (320.0, 640.0)
    );
    assert_eq!(
        AiPanel::mention_preview_image_size(320.0, 640.0, 2_000, 1_000),
        (320.0, 160.0)
    );
}

#[test]
fn mention_preview_image_size_shrinks_uniformly_to_available_height() {
    assert_eq!(
        AiPanel::mention_preview_image_size(320.0, 220.0, 1_000, 2_000),
        (110.0, 220.0)
    );
    assert_eq!(
        AiPanel::mention_preview_image_size(0.0, 220.0, 1_000, 2_000),
        (0.0, 0.0)
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
fn word_deletion_expands_to_the_whole_mention() {
    let source = "ask @document now";
    let mentions = parse_composer_mentions(source);
    let mention = &mentions[0];

    let backward = backward_word_deletion_start(source, mention.range.end)..mention.range.end;
    assert_eq!(expand_range_to_mentions(backward, &mentions), mention.range);

    let forward = mention.range.start..forward_word_deletion_end(source, mention.range.start);
    assert_eq!(expand_range_to_mentions(forward, &mentions), mention.range);
    assert_eq!(
        mention_aware_word_left(source, mention.range.end, &mentions),
        mention.range.start
    );
    assert_eq!(
        mention_aware_word_right(source, mention.range.start, &mentions),
        mention.range.end
    );
}

#[test]
fn composer_history_restores_mentions_and_page_edit_state() {
    let mentions = parse_composer_mentions("@page(12)");
    let digits = AiPanel::page_digits_range(&mentions[0]).unwrap();
    let before = ComposerSnapshot {
        content: "@page(12)".into(),
        selected_range: digits.clone(),
        selection_reversed: false,
        marked_range: Some(digits.clone()),
        mentions: mentions.clone(),
        active_page_number_edit: Some(ActivePageNumberEdit {
            mention_start: mentions[0].range.start,
            digits,
        }),
    };
    let after = ComposerSnapshot {
        content: "done".into(),
        selected_range: 4..4,
        selection_reversed: false,
        marked_range: None,
        mentions: Vec::new(),
        active_page_number_edit: None,
    };
    let mut history = EditHistory::default();
    history.record(before.clone(), &after, EditGroup::Isolated);

    assert_eq!(history.undo(after.clone()), Some(before.clone()));
    assert_eq!(history.redo(before), Some(after));
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
fn page_validation_defaults_and_clamps() {
    assert_eq!(validate_page_number("", 20), 1);
    assert_eq!(validate_page_number("0", 20), 1);
    assert_eq!(validate_page_number("12", 20), 12);
    assert_eq!(validate_page_number("99", 20), 20);
}

#[test]
fn empty_page_number_backspace_targets_the_whole_mention() {
    for source in ["@page(1)", "@page-image(1)"] {
        let mentions = parse_composer_mentions(source);
        let digits = AiPanel::page_digits_range(&mentions[0]).unwrap();
        let active = ActivePageNumberEdit {
            mention_start: mentions[0].range.start,
            digits: digits.start..digits.start,
        };

        assert_eq!(
            mention_range_for_empty_page_number_backspace(&active, active.digits.start, &mentions,),
            Some(mentions[0].range.clone())
        );
    }
}

#[test]
fn nonempty_page_number_backspace_keeps_digit_editing_active() {
    let mentions = parse_composer_mentions("@page(12)");
    let digits = AiPanel::page_digits_range(&mentions[0]).unwrap();
    let active = ActivePageNumberEdit {
        mention_start: mentions[0].range.start,
        digits: digits.clone(),
    };

    assert_eq!(
        mention_range_for_empty_page_number_backspace(&active, digits.end, &mentions),
        None
    );
}

#[test]
fn mention_trigger_requires_a_boundary() {
    assert_eq!(active_mention_range("ask @pa", 7, &[]), Some(4..7));
    assert_eq!(active_mention_range("mail@example", 12, &[]), None);
    assert_eq!(active_mention_range("(@page", 6, &[]), Some(1..6));
    assert_eq!(active_mention_range("ask @pa now", 11, &[]), None);
    assert_eq!(active_mention_range("ask @pa\n", 8, &[]), None);
}

#[test]
fn mention_trigger_accepts_an_explicit_mention_boundary() {
    let selection = parse_composer_mentions("@selection");
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
    let display = composer_display(source, &[]);

    assert_eq!(display.text, source);
    assert!(display.mentions.is_empty());
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
fn composer_display_maps_source_offsets_and_hides_canonical_syntax() {
    let source = "α @page(12) and @selection β";
    let mentions = parse_composer_mentions(source);
    let display = composer_display(source, &mentions);

    assert!(!display.text.contains('@'));
    assert!(!display.text.contains('('));
    assert!(display.text.contains("page"));
    assert!(display.text.contains("12"));
    assert!(display.text.contains("selection"));
    assert_eq!(display.mentions.len(), 2);
    assert!(display.mentions[0].number_range.is_some());
    assert!(display.mentions[1].number_range.is_none());
    assert_eq!(
        display.source_offset(display.mentions[1].range.start + 1),
        mentions[1].range.start
    );
    assert_eq!(
        display.source_offset(display.mentions[1].range.end - 1),
        mentions[1].range.end
    );
    for (display_mention, mention) in display.mentions.iter().zip(&mentions) {
        assert_eq!(
            display.text[display_mention.range.end..].chars().next(),
            Some('\u{00a0}')
        );
        assert_eq!(
            display.source_offset(display_mention.range.end),
            mention.range.end
        );
        assert_eq!(
            display.source_offset(display_mention.range.end + '\u{00a0}'.len_utf8()),
            mention.range.end
        );
    }

    for source_offset in [
        0,
        "α".len(),
        mentions[0].range.start,
        mentions[0].range.end,
        source.len(),
    ] {
        assert_eq!(
            display.source_offset(display.display_offset(source_offset)),
            source_offset
        );
    }
    let digits = AiPanel::page_digits_range(&mentions[0]).unwrap();
    for source_offset in digits.start..=digits.end {
        assert_eq!(
            display.source_offset(display.display_offset(source_offset)),
            source_offset
        );
    }
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
fn sent_message_pills_only_use_validated_occurrences() {
    let text = "literal @selection and attached @selection";
    let attached = find_mention_references(text)[1].clone();
    let reference = MentionReference::new(MentionKind::Selection, None);
    let exact = ChatMessage::user(
        text.to_string(),
        "literal @selection and attached".to_string(),
        vec![ContextBlock::new(
            MentionKind::Selection,
            None,
            "historical selection".to_string(),
        )],
        vec![reference],
    )
    .with_mention_occurrences(vec![MentionOccurrence::new(attached.0.clone(), attached.1)]);
    let mentions = display_mentions_for_message(&exact);
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].range, attached.0);
    let hovered = historical_preview_mention(
        &exact,
        (
            MentionHitbox {
                source_range: mentions[0].range.clone(),
                bounds: Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: size(px(10.0), px(10.0)),
                },
            },
            mentions[0].clone(),
        ),
    );
    assert_eq!(hovered.1.captured_text(), Some("historical selection"));

    let legacy = ChatMessage::user(
        text.to_string(),
        "literal @selection and attached".to_string(),
        Vec::new(),
        vec![reference],
    );
    assert!(display_mentions_for_message(&legacy).is_empty());
}

#[test]
fn sent_message_click_selection_is_utf8_and_mention_safe() {
    let text = "écho @page(2)\nnext line";
    let mentions = parse_composer_mentions(text);
    assert_eq!(selection_range_for_click(text, &mentions, 1, 2), 0..5);

    let mention = mentions.first().unwrap();
    assert_eq!(
        selection_range_for_click(text, &mentions, mention.range.start + 2, 1),
        mention.range.start..mention.range.start
    );
    assert_eq!(
        selection_range_for_click(text, &mentions, mention.range.start, 2),
        mention.range
    );

    let second_line = text.find("next").unwrap();
    assert_eq!(
        selection_range_for_click(text, &mentions, second_line, 3),
        second_line..text.len()
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
