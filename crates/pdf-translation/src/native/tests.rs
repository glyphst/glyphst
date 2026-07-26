use super::extraction::*;
use super::fusion::*;

use std::collections::BTreeMap;

use super::*;

#[test]
fn reconciles_provider_bbox_across_split_native_blocks() {
    let provider = BoundingBox::new(66.0, 449.0, 293.0, 748.0);
    let native = [
        BoundingBox::new(70.0, 451.0, 289.0, 489.0),
        BoundingBox::new(70.0, 493.0, 291.0, 690.0),
        BoundingBox::new(71.0, 695.0, 291.0, 773.0),
    ];

    assert_eq!(
        reconciled_bbox(provider, native.into_iter()),
        Some(BoundingBox::new(66.0, 449.0, 293.0, 773.0))
    );
}

#[test]
fn rejects_unrelated_native_geometry() {
    let provider = BoundingBox::new(66.0, 449.0, 293.0, 748.0);
    let native = [BoundingBox::new(310.0, 440.0, 525.0, 750.0)];

    assert_eq!(reconciled_bbox(provider, native.into_iter()), None);
}

#[test]
fn malformed_tall_glyph_bounds_do_not_displace_line_geometry() {
    let raw_bbox = BoundingBox::new(30.0, 20.0, 34.0, 40.0);

    let bbox = baseline_normalized_line_bbox(raw_bbox, 30.0, 10.0);

    assert_eq!(bbox, BoundingBox::new(30.0, 21.8, 34.0, 32.2));
}

#[test]
fn ordinary_text_keeps_baseline_normalized_vertical_bounds() {
    let raw_bbox = BoundingBox::new(30.0, 20.0, 80.0, 40.0);

    let bbox = baseline_normalized_line_bbox(raw_bbox, 30.0, 10.0);

    assert_eq!(bbox, BoundingBox::new(30.0, 21.8, 80.0, 32.2));
}

#[test]
fn merges_small_cap_fragments_back_into_their_visual_line() {
    let style = TextStyle {
        font_size: Some(10.0),
        ..TextStyle::default()
    };
    let line = |bbox: BoundingBox, text: &str| Line {
        bbox,
        text: text.to_string(),
        segments: vec![Segment {
            kind: SegmentKind::Text,
            text: text.to_string(),
            bbox,
            style: style.clone(),
        }],
        baseline_y: Some(bbox.y1 - 2.0),
        line_height: Some(bbox.height()),
    };
    let lines = vec![
        line(
            BoundingBox::new(10.0, 10.0, 90.0, 20.0),
            "effectiveness of W",
        ),
        line(BoundingBox::new(89.0, 12.0, 105.0, 18.0), "ORD"),
        line(BoundingBox::new(104.0, 12.0, 120.0, 18.0), "TOUR"),
    ];

    let merged = merge_inline_native_lines(lines);

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].text, "effectiveness of WORDTOUR");
    assert_eq!(merged[0].segments.len(), 3);
}

#[test]
fn distinguishes_superscripts_from_baseline_small_caps() {
    let segment = |text: &str, bbox: BoundingBox, font_size: f32| Segment {
        kind: SegmentKind::Text,
        text: text.to_string(),
        bbox,
        style: TextStyle {
            font_size: Some(font_size),
            ..TextStyle::default()
        },
    };
    let mut formula = vec![
        segment("n", BoundingBox::new(0.0, 10.0, 6.0, 21.0), 11.0),
        segment("3", BoundingBox::new(6.0, 8.0, 10.0, 16.0), 8.0),
        segment("+n", BoundingBox::new(10.0, 10.0, 22.0, 21.0), 11.0),
    ];
    annotate_baseline_positions(&mut formula);
    assert_eq!(
        formula[1].style.baseline_position,
        BaselinePosition::Superscript
    );

    let mut small_caps = vec![
        segment("W", BoundingBox::new(0.0, 10.0, 7.0, 21.0), 11.0),
        segment("ORD", BoundingBox::new(7.0, 12.0, 20.0, 20.5), 8.5),
        segment("T", BoundingBox::new(20.0, 10.0, 27.0, 21.0), 11.0),
    ];
    annotate_baseline_positions(&mut small_caps);
    assert_eq!(
        small_caps[1].style.baseline_position,
        BaselinePosition::Normal
    );
}

#[test]
fn splits_indented_prose_paragraphs_without_dropping_lines() {
    let style = TextStyle {
        font_size: Some(10.0),
        font_class: Some(FontFamilyClass::Serif),
        ..TextStyle::default()
    };
    let line = |index: usize, x0: f32, x1: f32, text: &str| {
        let y0 = 10.0 + index as f32 * 12.0;
        let bbox = BoundingBox::new(x0, y0, x1, y0 + 10.0);
        Line {
            bbox,
            text: text.to_string(),
            segments: vec![Segment {
                kind: SegmentKind::Text,
                text: text.to_string(),
                bbox,
                style: style.clone(),
            }],
            baseline_y: Some(y0 + 8.0),
            line_height: Some(10.0),
        }
    };
    let lines = vec![
        line(
            0,
            10.0,
            100.0,
            "This is a sufficiently long opening paragraph",
        ),
        line(
            1,
            10.0,
            100.0,
            "whose visual lines continue at the left edge",
        ),
        line(2, 10.0, 82.0, "and finish with a complete sentence."),
        line(3, 18.0, 100.0, "The second paragraph begins with an indent"),
        line(
            4,
            10.0,
            100.0,
            "and returns to the dominant left edge before",
        ),
        line(5, 10.0, 88.0, "ending without losing any source text."),
    ];
    let block = native_block_from_lines(lines, 0.0, 0.0, 0.0).unwrap();

    let paragraphs = split_native_paragraphs(block);

    assert_eq!(paragraphs.len(), 2);
    assert!(paragraphs[0].text.ends_with("complete sentence."));
    assert!(paragraphs[1].text.starts_with("The second paragraph"));
    assert_eq!(
        paragraphs
            .iter()
            .flat_map(|paragraph| paragraph.lines.iter())
            .count(),
        6
    );
    assert!((paragraphs[1].lines[0].bbox.x0 - paragraphs[1].bbox.x0) >= 7.5);
}

#[test]
fn inline_merge_restores_spaces_at_semantic_fragment_boundaries() {
    let style = TextStyle {
        font_size: Some(10.0),
        ..TextStyle::default()
    };
    let line = |bbox: BoundingBox, text: &str| Line {
        bbox,
        text: text.to_string(),
        segments: vec![Segment {
            kind: SegmentKind::Text,
            text: text.to_string(),
            bbox,
            style: style.clone(),
        }],
        baseline_y: Some(bbox.y1 - 2.0),
        line_height: Some(bbox.height()),
    };
    let lines = vec![
        line(BoundingBox::new(10.0, 10.0, 45.0, 20.0), "Figure"),
        line(BoundingBox::new(44.5, 10.0, 50.0, 20.0), "2"),
        line(BoundingBox::new(49.5, 10.0, 75.0, 20.0), "shows"),
        line(BoundingBox::new(74.5, 10.0, 85.0, 20.0), "W"),
        line(BoundingBox::new(84.5, 10.0, 103.0, 20.0), "ORD"),
        line(BoundingBox::new(102.5, 10.0, 110.0, 20.0), "T"),
        line(BoundingBox::new(109.5, 10.0, 130.0, 20.0), "OUR"),
        line(BoundingBox::new(129.5, 10.0, 155.0, 20.0), "aligns"),
    ];

    let merged = merge_inline_native_lines(lines);

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].text, "Figure 2 shows WORDTOUR aligns");
    let numeric_identifier = line(BoundingBox::new(10.0, 30.0, 50.0, 40.0), "20news");
    assert_eq!(
        merge_inline_native_lines(vec![numeric_identifier])[0].text,
        "20news"
    );
}

#[test]
fn segment_spacing_separates_prose_from_inline_math() {
    let prose = TextStyle {
        font_class: Some(FontFamilyClass::Serif),
        ..TextStyle::default()
    };
    let math = TextStyle {
        font_class: Some(FontFamilyClass::Math),
        ..TextStyle::default()
    };
    let segment = |text: &str, x0: f32, x1: f32, style: TextStyle| Segment {
        kind: SegmentKind::Text,
        text: text.to_string(),
        bbox: BoundingBox::new(x0, 10.0, x1, 20.0),
        style,
    };
    let mut segments = vec![
        segment("ordering", 10.0, 45.0, prose.clone()),
        segment("σ", 45.0, 52.0, math.clone()),
        segment("and", 52.0, 68.0, prose.clone()),
        segment("τ", 68.0, 75.0, math.clone()),
    ];

    restore_segment_spaces(&mut segments);

    assert_eq!(
        segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>(),
        "ordering σ and τ"
    );

    let mut formula = vec![
        segment("n", 10.0, 16.0, math.clone()),
        segment("3", 17.8, 21.0, math.clone()),
        segment("+", 21.0, 25.0, math.clone()),
        segment("n", 25.0, 31.0, math.clone()),
        segment("2", 32.8, 36.0, math.clone()),
        segment("d", 36.0, 42.0, math),
    ];
    restore_segment_spaces(&mut formula);
    assert_eq!(
        formula
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>(),
        "n3+n2d"
    );
}

#[test]
fn ocr_fusion_keeps_native_text_and_style_while_adding_missing_regions() {
    let native_style = TextStyle {
        font_family: Some("Source Serif".to_string()),
        font_class: Some(FontFamilyClass::Serif),
        font_size: Some(10.0),
        bold: true,
        ..TextStyle::default()
    };
    let native = fusion_document(
        "native",
        vec![fusion_block(
            "native-body",
            BoundingBox::new(10.0, 30.0, 90.0, 60.0),
            BlockRole::Body,
            "Native text",
            native_style.clone(),
        )],
    );
    let ocr = fusion_document(
        "ocr",
        vec![
            fusion_block(
                "ocr-body",
                BoundingBox::new(10.0, 30.0, 90.0, 60.0),
                BlockRole::Abstract,
                "",
                TextStyle::default(),
            ),
            fusion_block(
                "ocr-title",
                BoundingBox::new(10.0, 5.0, 90.0, 20.0),
                BlockRole::Title,
                "Missing title",
                TextStyle::default(),
            ),
        ],
    );

    let fused = fuse_native_with_ocr(native, &ocr);
    let native_body = fused.pages[0]
        .blocks
        .iter()
        .find(|block| block.id == "native-body")
        .unwrap();
    let added_title = fused.pages[0]
        .blocks
        .iter()
        .find(|block| block.id == "ocr-ocr-title")
        .unwrap();

    assert_eq!(native_body.content.text, "Native text");
    assert_eq!(native_body.content.segments[0].style, native_style);
    assert_eq!(native_body.role, BlockRole::Abstract);
    assert_eq!(added_title.role, BlockRole::Title);
    assert_eq!(added_title.content.text, "Missing title");
    assert!(added_title.tags.contains(&"ocr_only".to_string()));
}

#[test]
fn ocr_topology_splits_a_native_prompt_into_ordered_members() {
    let style = TextStyle {
        font_size: Some(10.0),
        ..TextStyle::default()
    };
    let native = fusion_document(
        "native",
        vec![fusion_multiline_block(
            "native-prompt",
            BlockRole::Body,
            vec![
                (
                    BoundingBox::new(10.0, 10.0, 55.0, 18.0),
                    "Statements about Klaus",
                    style.clone(),
                ),
                (
                    BoundingBox::new(10.0, 20.0, 55.0, 28.0),
                    "1. Klaus writes a paper",
                    style.clone(),
                ),
                (
                    BoundingBox::new(10.0, 30.0, 55.0, 38.0),
                    "2. Klaus reads a book",
                    style.clone(),
                ),
                (
                    BoundingBox::new(10.0, 40.0, 55.0, 48.0),
                    "What can you infer?",
                    style,
                ),
            ],
        )],
    );
    let texts = [
        "Statements about Klaus",
        "1. Klaus writes a paper",
        "2. Klaus reads a book",
        "What can you infer?",
    ];
    let mut ocr_blocks = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let mut block = fusion_block(
                &format!("ocr-{index}"),
                BoundingBox::new(
                    10.0,
                    10.0 + index as f32 * 10.0,
                    55.0,
                    18.0 + index as f32 * 10.0,
                ),
                BlockRole::Body,
                text,
                TextStyle::default(),
            );
            block.order = index;
            block.reading_order = index;
            block.structure = Some(test_structure(0.98));
            block
        })
        .collect::<Vec<_>>();
    let ocr = fusion_document("paddle", std::mem::take(&mut ocr_blocks));

    let fused = fuse_native_with_ocr_report(native, &ocr);
    let page = &fused.document.pages[0];

    assert_eq!(fused.report.pages[0].accepted_splits, 1);
    assert_eq!(page.blocks.len(), 4);
    assert_eq!(
        page.blocks
            .iter()
            .map(|block| block.content.text.as_str())
            .collect::<Vec<_>>(),
        texts
    );
    assert!(
        page.blocks
            .iter()
            .all(|block| block.role == BlockRole::Prompt)
    );
    assert_eq!(
        page.blocks[1].structure.as_ref().unwrap().kind,
        StructureKind::OrderedListItem
    );
    assert_eq!(
        page.blocks[1].structure.as_ref().unwrap().marker.as_deref(),
        Some("1.")
    );
    assert_eq!(
        page.blocks[3].structure.as_ref().unwrap().kind,
        StructureKind::PromptQuestion
    );
}

#[test]
fn ocr_topology_splits_semantic_fields_without_splitting_visual_wraps() {
    let style = TextStyle {
        font_size: Some(8.0),
        ..TextStyle::default()
    };
    let native = fusion_document(
        "native",
        vec![fusion_multiline_block(
            "native-fields",
            // A coarse native heuristic can misclassify the whole region
            // before OCR exposes its prompt-field topology.
            BlockRole::Dialogue,
            vec![
                (
                    BoundingBox::new(20.0, 10.0, 75.0, 18.0),
                    "[Summary Description]",
                    style.clone(),
                ),
                (
                    BoundingBox::new(20.0, 20.0, 92.0, 28.0),
                    "Status: a deliberately long value that",
                    style.clone(),
                ),
                (
                    BoundingBox::new(20.0, 30.0, 72.0, 38.0),
                    "wraps onto a second visual line.",
                    style.clone(),
                ),
                (
                    BoundingBox::new(20.0, 40.0, 88.0, 48.0),
                    "Observation: another semantic field.",
                    style.clone(),
                ),
                (
                    BoundingBox::new(20.0, 50.0, 70.0, 58.0),
                    "What should happen next?",
                    style,
                ),
            ],
        )],
    );
    let candidates = [
        (
            BoundingBox::new(20.0, 10.0, 75.0, 18.0),
            "[Summary Description]",
        ),
        (
            BoundingBox::new(20.0, 20.0, 92.0, 38.0),
            "Status: a deliberately long value that wraps onto a second visual line.",
        ),
        (
            BoundingBox::new(20.0, 40.0, 88.0, 48.0),
            "Observation: another semantic field.",
        ),
        (
            BoundingBox::new(20.0, 50.0, 70.0, 58.0),
            "What should happen next?",
        ),
    ];
    let ocr_blocks = candidates
        .iter()
        .enumerate()
        .map(|(index, (bbox, text))| {
            let mut block = fusion_block(
                &format!("unlimited-{index}"),
                *bbox,
                BlockRole::Body,
                text,
                TextStyle::default(),
            );
            block.order = index;
            block.reading_order = index;
            block.source.provider = "unlimited".to_string();
            block.structure = Some(BlockStructure {
                provider: "unlimited".to_string(),
                confidence: 0.99,
                ..test_structure(0.99)
            });
            block
        })
        .collect::<Vec<_>>();
    let ocr = fusion_document("unlimited", ocr_blocks);

    let fusion = fuse_native_with_ocr_report(native, &ocr);
    let blocks = &fusion.document.pages[0].blocks;

    assert_eq!(
        fusion.report.pages[0].accepted_splits, 1,
        "{:#?}",
        fusion.report
    );
    assert_eq!(fusion.report.pages[0].native_fallbacks, 0);
    assert_eq!(blocks.len(), 4);
    assert!(blocks.iter().all(|block| block.role == BlockRole::Prompt));
    assert_eq!(blocks[1].content.lines.len(), 2);
    assert_eq!(
        blocks[1].content.text,
        "Status: a deliberately long value that\nwraps onto a second visual line."
    );
    assert_eq!(
        blocks[3].structure.as_ref().unwrap().kind,
        StructureKind::PromptQuestion
    );
    assert!(
        fusion.report.pages[0]
            .decisions
            .iter()
            .any(|decision| decision.reason == "validated_ocr_structure:unlimited")
    );
}

#[test]
fn coarse_ocr_semantic_newlines_expand_without_promoting_visual_wraps() {
    let style = TextStyle {
        font_size: Some(8.0),
        ..TextStyle::default()
    };
    let native = fusion_document(
        "native",
        vec![fusion_multiline_block(
            "native-coarse-fields",
            BlockRole::Dialogue,
            vec![
                (
                    BoundingBox::new(20.0, 10.0, 75.0, 18.0),
                    "[Summary Description]",
                    style.clone(),
                ),
                (
                    BoundingBox::new(20.0, 20.0, 92.0, 28.0),
                    "Status: a deliberately long value that",
                    style.clone(),
                ),
                (
                    BoundingBox::new(20.0, 30.0, 72.0, 38.0),
                    "wraps onto a second visual line.",
                    style.clone(),
                ),
                (
                    BoundingBox::new(20.0, 40.0, 88.0, 48.0),
                    "Observation: another semantic field.",
                    style.clone(),
                ),
                (
                    BoundingBox::new(20.0, 50.0, 70.0, 58.0),
                    "What should happen next?",
                    style,
                ),
            ],
        )],
    );
    let bbox = BoundingBox::new(20.0, 10.0, 92.0, 58.0);
    let mut coarse_ocr = fusion_block(
        "unlimited-coarse",
        bbox,
        BlockRole::Body,
        "[Summary Description]\nStatus: a deliberately long value that\nwraps onto a second visual line.\nObservation: another semantic field.\nWhat should happen next?",
        TextStyle::default(),
    );
    coarse_ocr.source.provider = "unlimited".to_string();
    coarse_ocr.structure = Some(BlockStructure {
        provider: "unlimited".to_string(),
        confidence: 0.99,
        ..test_structure(0.99)
    });
    let ocr = fusion_document("unlimited", vec![coarse_ocr]);

    let fusion = fuse_native_with_ocr_report(native, &ocr);
    let blocks = &fusion.document.pages[0].blocks;

    assert_eq!(fusion.report.pages[0].accepted_splits, 1);
    assert_eq!(fusion.report.pages[0].native_fallbacks, 0);
    assert_eq!(blocks.len(), 4);
    assert!(blocks.iter().all(|block| block.role == BlockRole::Prompt));
    assert_eq!(blocks[1].content.lines.len(), 2);
    assert_eq!(
        blocks[1].content.text,
        "Status: a deliberately long value that\nwraps onto a second visual line."
    );
    assert!(
        blocks
            .iter()
            .all(|block| block.tags.contains(&"ocr_topology_fused".to_string()))
    );
}

#[test]
fn ocr_topology_merges_caption_fragments_without_losing_native_styles() {
    let red = TextStyle {
        font_size: Some(9.0),
        color_argb: Some(0xffaa_0000),
        ..TextStyle::default()
    };
    let blue = TextStyle {
        font_size: Some(9.0),
        color_argb: Some(0xff00_00aa),
        italic: true,
        ..TextStyle::default()
    };
    let native = fusion_document(
        "native",
        vec![
            fusion_block(
                "caption-a",
                BoundingBox::new(10.0, 10.0, 45.0, 18.0),
                BlockRole::Body,
                "Figure 2: A useful",
                red.clone(),
            ),
            fusion_block(
                "caption-b",
                BoundingBox::new(10.0, 20.0, 55.0, 28.0),
                BlockRole::Body,
                "caption continues.",
                blue.clone(),
            ),
        ],
    );
    let mut ocr_caption = fusion_block(
        "ocr-caption",
        BoundingBox::new(10.0, 10.0, 55.0, 28.0),
        BlockRole::FigureCaption,
        "Figure 2: A useful caption continues.",
        TextStyle::default(),
    );
    ocr_caption.structure = Some(BlockStructure {
        kind: StructureKind::Caption,
        ..test_structure(0.98)
    });
    let ocr = fusion_document("paddle", vec![ocr_caption]);

    let fused = fuse_native_with_ocr_report(native, &ocr);
    let caption = &fused.document.pages[0].blocks[0];

    assert_eq!(fused.report.pages[0].accepted_merges, 1);
    assert_eq!(fused.document.pages[0].blocks.len(), 1);
    assert_eq!(caption.role, BlockRole::FigureCaption);
    assert!(caption.content.text.contains("Figure 2: A useful"));
    assert!(caption.content.text.contains("caption continues."));
    assert!(
        caption
            .content
            .segments
            .iter()
            .any(|segment| segment.style == red)
    );
    assert!(
        caption
            .content
            .segments
            .iter()
            .any(|segment| segment.style == blue)
    );
}

#[test]
fn ocr_topology_preserves_bullet_markers_as_structure() {
    let native = fusion_document(
        "native",
        vec![fusion_multiline_block(
            "native-list",
            BlockRole::Body,
            vec![
                (
                    BoundingBox::new(10.0, 10.0, 55.0, 18.0),
                    "• First item",
                    TextStyle::default(),
                ),
                (
                    BoundingBox::new(10.0, 20.0, 55.0, 28.0),
                    "• Second item",
                    TextStyle::default(),
                ),
            ],
        )],
    );
    let mut first = fusion_block(
        "ocr-first",
        BoundingBox::new(10.0, 10.0, 55.0, 18.0),
        BlockRole::Body,
        "• First item",
        TextStyle::default(),
    );
    first.structure = Some(test_structure(0.98));
    let mut second = fusion_block(
        "ocr-second",
        BoundingBox::new(10.0, 20.0, 55.0, 28.0),
        BlockRole::Body,
        "• Second item",
        TextStyle::default(),
    );
    second.order = 1;
    second.reading_order = 1;
    second.structure = Some(test_structure(0.98));
    let ocr = fusion_document("paddle", vec![first, second]);

    let fused = fuse_native_with_ocr_report(native, &ocr);

    assert_eq!(fused.report.pages[0].accepted_splits, 1);
    assert!(fused.document.pages[0].blocks.iter().all(|block| {
        let structure = block.structure.as_ref().unwrap();
        structure.kind == StructureKind::BulletListItem && structure.marker.as_deref() == Some("•")
    }));
}

#[test]
fn ocr_topology_promotes_detached_markers_and_keeps_truncated_tail_lines() {
    let native = fusion_document(
        "native",
        vec![fusion_multiline_block(
            "native-list",
            BlockRole::Body,
            vec![
                (
                    BoundingBox::new(12.0, 10.0, 55.0, 18.0),
                    "Memory: Who is running for",
                    TextStyle::default(),
                ),
                (
                    BoundingBox::new(9.0, 12.0, 10.0, 15.0),
                    "•",
                    TextStyle::default(),
                ),
                (
                    BoundingBox::new(12.0, 20.0, 25.0, 28.0),
                    "mayor?",
                    TextStyle::default(),
                ),
                (
                    BoundingBox::new(12.0, 32.0, 55.0, 40.0),
                    "Reflections: why?",
                    TextStyle::default(),
                ),
                (
                    BoundingBox::new(9.0, 34.0, 10.0, 37.0),
                    "•",
                    TextStyle::default(),
                ),
            ],
        )],
    );
    let mut memory = fusion_block(
        "ocr-memory",
        BoundingBox::new(9.0, 9.0, 58.0, 29.0),
        BlockRole::Body,
        "- Memory: Who is running for",
        TextStyle::default(),
    );
    memory.structure = Some(test_structure(0.98));
    let mut reflections = fusion_block(
        "ocr-reflections",
        BoundingBox::new(9.0, 31.0, 58.0, 41.0),
        BlockRole::Body,
        "- Reflections: why?",
        TextStyle::default(),
    );
    reflections.order = 1;
    reflections.reading_order = 1;
    reflections.structure = Some(test_structure(0.98));
    let ocr = fusion_document("paddle", vec![memory, reflections]);

    let fused = fuse_native_with_ocr_report(native, &ocr);
    let blocks = &fused.document.pages[0].blocks;

    assert_eq!(fused.report.pages[0].accepted_splits, 1);
    assert_eq!(blocks.len(), 2);
    assert!(blocks[0].content.text.starts_with("•\nMemory:"));
    assert!(blocks[0].content.text.ends_with("mayor?"));
    assert!(blocks[1].content.text.starts_with("•\nReflections:"));
    assert!(
        blocks
            .iter()
            .all(|block| { block.structure.as_ref().unwrap().marker.as_deref() == Some("•") })
    );
    assert_eq!(fused.report.pages[0].decisions[0].native_coverage, 1.0);
}

#[test]
fn low_confidence_ocr_topology_fails_closed_to_native_block() {
    let native = fusion_document(
        "native",
        vec![fusion_multiline_block(
            "native-list",
            BlockRole::Body,
            vec![
                (
                    BoundingBox::new(10.0, 10.0, 55.0, 18.0),
                    "1. First item",
                    TextStyle::default(),
                ),
                (
                    BoundingBox::new(10.0, 20.0, 55.0, 28.0),
                    "2. Second item",
                    TextStyle::default(),
                ),
            ],
        )],
    );
    let mut first = fusion_block(
        "ocr-first",
        BoundingBox::new(10.0, 10.0, 55.0, 18.0),
        BlockRole::Body,
        "1. First item",
        TextStyle::default(),
    );
    first.structure = Some(test_structure(0.94));
    let mut second = fusion_block(
        "ocr-second",
        BoundingBox::new(10.0, 20.0, 55.0, 28.0),
        BlockRole::Body,
        "2. Second item",
        TextStyle::default(),
    );
    second.structure = Some(test_structure(0.94));
    let ocr = fusion_document("paddle", vec![first, second]);

    let fused = fuse_native_with_ocr_report(native, &ocr);

    assert_eq!(fused.report.pages[0].accepted_splits, 0);
    assert_eq!(fused.report.pages[0].native_fallbacks, 1);
    assert_eq!(fused.document.pages[0].blocks.len(), 1);
    assert_eq!(fused.document.pages[0].blocks[0].id, "native-list");
    assert!(fused.document.pages[0].blocks[0].structure.is_none());
}

#[test]
fn ocr_code_region_is_demoted_for_non_monospace_prompt_prose() {
    let bbox = BoundingBox::new(10.0, 20.0, 90.0, 80.0);
    let mut native_block = fusion_block(
        "native-prompt",
        bbox,
        BlockRole::Body,
        "[Agent’s Summary Description]\nObservation: John saw Eddy taking a short walk.\nShould John react to the observation?",
        TextStyle::default(),
    );
    native_block
        .metadata
        .insert("monospaced_ratio".to_string(), json!(0.0));
    let native = fusion_document("native", vec![native_block]);
    let mut ocr = fusion_document("paddleocr-vl", Vec::new());
    ocr.pages[0].protected_regions.push(ProtectedRegion {
        id: "ocr-code".to_string(),
        page_index: 0,
        kind: ProtectedRegionKind::Code,
        bbox,
        source: "paddleocr-vl".to_string(),
        confidence: 0.99,
        reason: "provider_non_text:code".to_string(),
    });

    let fused = fuse_native_with_ocr(native, &ocr);
    let prompt = &fused.pages[0].blocks[0];

    assert_eq!(prompt.role, BlockRole::Prompt);
    assert!(prompt.policy.translate);
    assert!(!prompt.policy.locked);
    assert!(
        prompt
            .tags
            .contains(&"ocr_code_reclassified_as_prompt".to_string())
    );
    assert!(fused.pages[0].protected_regions.is_empty());
}

#[test]
fn ocr_code_region_remains_locked_for_executable_code() {
    let bbox = BoundingBox::new(10.0, 20.0, 90.0, 80.0);
    let native = fusion_document(
        "native",
        vec![fusion_block(
            "native-code",
            bbox,
            BlockRole::Body,
            "fn main() { println!(\"hello\"); }",
            TextStyle::default(),
        )],
    );
    let mut ocr = fusion_document("paddleocr-vl", Vec::new());
    ocr.pages[0].protected_regions.push(ProtectedRegion {
        id: "ocr-code".to_string(),
        page_index: 0,
        kind: ProtectedRegionKind::Code,
        bbox,
        source: "paddleocr-vl".to_string(),
        confidence: 0.99,
        reason: "provider_non_text:code".to_string(),
    });

    let fused = fuse_native_with_ocr(native, &ocr);
    let code = &fused.pages[0].blocks[0];

    assert_eq!(code.role, BlockRole::Code);
    assert!(!code.policy.translate);
    assert!(code.policy.locked);
    assert_eq!(fused.pages[0].protected_regions.len(), 1);
}

#[test]
fn native_dialogue_is_classified_by_speaker_turns() {
    let bbox = BoundingBox::new(10.0, 20.0, 90.0, 80.0);
    let native = fusion_document(
        "native",
        vec![fusion_block(
            "native-dialogue",
            bbox,
            BlockRole::Body,
            "Interviewer: How are you?\nKlaus Mueller: I am doing well.\nInterviewer: What are you working on?",
            TextStyle::default(),
        )],
    );
    let ocr = fusion_document("ocr", Vec::new());

    let fused = fuse_native_with_ocr(native, &ocr);

    assert_eq!(fused.pages[0].blocks[0].role, BlockRole::Dialogue);
    assert_eq!(
        fused.pages[0].blocks[0]
            .metadata
            .get("semantic_line_structure"),
        Some(&json!("speaker_turns"))
    );
}

fn fusion_document(provider: &str, blocks: Vec<Block>) -> DocumentV1 {
    DocumentV1::new(
        DocumentMetadata {
            document_id: provider.to_string(),
            source_path: "fixture.pdf".into(),
            provider: provider.to_string(),
            provider_version: "1".to_string(),
            extra: BTreeMap::new(),
        },
        vec![Page {
            page_index: 0,
            geometry: PageGeometry::new(100.0, 100.0),
            blocks,
            protected_regions: Vec::new(),
            metadata: BTreeMap::new(),
        }],
    )
}

fn fusion_block(
    id: &str,
    bbox: BoundingBox,
    role: BlockRole,
    text: &str,
    style: TextStyle,
) -> Block {
    let segment = Segment {
        kind: SegmentKind::Text,
        text: text.to_string(),
        bbox,
        style,
    };
    Block {
        id: id.to_string(),
        page_index: 0,
        order: 0,
        reading_order: 0,
        kind: BlockKind::Text,
        role,
        bbox,
        content: BlockContent {
            text: text.to_string(),
            lines: vec![Line {
                bbox,
                text: text.to_string(),
                segments: vec![segment.clone()],
                baseline_y: Some(bbox.y1 - 2.0),
                line_height: Some(bbox.height()),
            }],
            segments: vec![segment],
            markdown: None,
            html: None,
        },
        policy: policy_for_role(role),
        continuation: None,
        structure: None,
        source: SourceProvenance {
            provider: "fixture".to_string(),
            raw_label: "text".to_string(),
            raw_bbox: bbox,
            raw_path: None,
        },
        tags: Vec::new(),
        metadata: BTreeMap::new(),
    }
}

fn fusion_multiline_block(
    id: &str,
    role: BlockRole,
    lines: Vec<(BoundingBox, &str, TextStyle)>,
) -> Block {
    let bbox = lines
        .iter()
        .map(|(bbox, _, _)| *bbox)
        .reduce(BoundingBox::union)
        .unwrap();
    let content_lines = lines
        .into_iter()
        .map(|(bbox, text, style)| {
            let segment = Segment {
                kind: SegmentKind::Text,
                text: text.to_string(),
                bbox,
                style,
            };
            Line {
                bbox,
                text: text.to_string(),
                segments: vec![segment],
                baseline_y: Some(bbox.y1 - 2.0),
                line_height: Some(bbox.height()),
            }
        })
        .collect::<Vec<_>>();
    let text = content_lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let segments = content_lines
        .iter()
        .flat_map(|line| line.segments.iter().cloned())
        .collect();
    let mut block = fusion_block(id, bbox, role, &text, TextStyle::default());
    block.content = BlockContent {
        text,
        lines: content_lines,
        segments,
        markdown: None,
        html: None,
    };
    block
}

fn test_structure(confidence: f32) -> BlockStructure {
    BlockStructure {
        kind: StructureKind::Region,
        flow: TextFlow::Reflow,
        group_id: None,
        group_bbox: None,
        marker: None,
        provider: "paddle".to_string(),
        confidence,
    }
}
