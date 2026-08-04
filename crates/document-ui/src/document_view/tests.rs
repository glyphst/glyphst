use std::{
    collections::HashSet,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use super::{
    DocumentPage, DocumentView, Glyph, RegionDragState, RegionImageExport, RenderCache, RenderKey,
    SearchMatch, SearchState, SurfaceRenderKeys, TRANSLATION_HOVER_TOOLS_OVERLAY_PRIORITY,
    TRANSLATION_PANEL_DEFAULT_RIGHT, TRANSLATION_PANEL_DEFAULT_TOP, TRANSLATION_PANEL_DOCK_DELAY,
    TRANSLATION_PANEL_DRAG_OVERLAY_PRIORITY, TRANSLATION_PANEL_MARGIN,
    TRANSLATION_PANEL_OVERLAY_PRIORITY, TRANSLATION_PANEL_PREFERRED_WIDTH,
    TRANSLATION_PANEL_RETRACT_DELAY, TRANSLATION_PANEL_SNAP_ZONE, TextChunk, TextFormatBlock,
    TextIndexState, TextLine, TranslationCardState, TranslationHoverAnimation,
    TranslationPanelSide, TranslationStatus, TranslationViewRole, clamp_translation_panel_edge_y,
    clamp_translation_panel_position, clear_copied_region_selection,
    default_translation_panel_position, page_render::should_start_viewport_render,
    reset_translation_cards_for_document_replacement, retain_valid_annotation_id,
    translation_pair_highlights_enabled, translation_panel_docked_x, translation_panel_snap_side,
    translation_panel_width, translation_toolbar_enabled,
};
use glyphst_ai::ai_chat::{MentionKind, MentionReference};
use glyphst_core::{
    DocumentLayout, DocumentRotation, EpubLayout, HighlightColor, PageIndex, PageRegion,
    RegionImageReference,
};
use glyphst_library::{Highlight, HighlightRect, Note};
use glyphst_translation::LanguageCode;
use gpui::{Bounds, Modifiers, ScrollWheelEvent, point, px, size};
use image::{Frame, GenericImageView, RgbaImage};
use mupdf::{Point, Quad, Rect};
use smallvec::smallvec;

#[test]
fn annotation_reconciliation_clears_only_stale_ids() {
    let valid_ids = HashSet::from([7, 11]);
    let mut retained = Some(7);
    let mut removed = Some(9);
    let mut empty = None;

    assert!(!retain_valid_annotation_id(&mut retained, &valid_ids));
    assert_eq!(retained, Some(7));
    assert!(retain_valid_annotation_id(&mut removed, &valid_ids));
    assert_eq!(removed, None);
    assert!(!retain_valid_annotation_id(&mut empty, &valid_ids));
}

#[test]
fn page_counter_uses_generated_pages_for_any_document_format() {
    assert_eq!(DocumentView::page_counter_label(3, 12), "3 / 12");
}

#[test]
fn synchronized_scroll_preserves_screen_distance_across_zoom_levels() {
    let screen_delta = 96.0;
    for scale in [0.5, 1.0, 1.75, 3.0] {
        let document_delta = DocumentView::synchronized_document_delta(screen_delta, scale);
        assert!((document_delta * scale - screen_delta).abs() < f32::EPSILON);
    }
}

#[test]
fn completed_text_index_does_not_restart() {
    let mut state = TextIndexState::Pending;

    assert!(state.begin());
    assert_eq!(state, TextIndexState::Indexing);
    assert!(!state.begin());

    state.settle(true);
    assert_eq!(state, TextIndexState::Ready);
    assert!(!state.begin());
}

#[test]
fn failed_text_index_does_not_retry_implicitly() {
    let mut state = TextIndexState::Pending;

    assert!(state.begin());
    state.settle(false);

    assert_eq!(state, TextIndexState::Failed);
    assert!(!state.begin());
}

#[test]
fn search_reveal_after_indexing_is_consumed_once() {
    let mut search = SearchState {
        query: "needle".into(),
        ..Default::default()
    };

    search.update_reveal_after_indexing(true);

    assert!(search.take_reveal_after_indexing());
    assert!(!search.take_reveal_after_indexing());
}

#[test]
fn manual_navigation_cancels_search_reveal_after_indexing() {
    let mut search = SearchState {
        query: "needle".into(),
        ..Default::default()
    };

    search.update_reveal_after_indexing(true);
    search.cancel_reveal_after_indexing();

    assert!(!search.take_reveal_after_indexing());
}

#[test]
fn empty_search_does_not_request_reveal_after_indexing() {
    let mut search = SearchState::default();

    search.update_reveal_after_indexing(true);

    assert!(!search.take_reveal_after_indexing());
}

#[test]
fn thumbnail_overlays_are_grouped_by_page_and_active_match() {
    let mut glyphs = vec![
        test_glyph('a', 0.0, 5.0),
        test_glyph('b', 5.0, 10.0),
        test_glyph('c', 0.0, 4.0),
        test_glyph('d', 2.0, 8.0),
    ];
    glyphs[2].line_idx = 1;
    glyphs[2].quad = Quad::new(
        Point::new(0.0, 10.0),
        Point::new(4.0, 10.0),
        Point::new(0.0, 16.0),
        Point::new(4.0, 16.0),
    );
    glyphs[2].line_rect = Rect::new(0.0, 10.0, 4.0, 16.0);
    glyphs[3].page_idx = 1;

    let matches = vec![
        SearchMatch {
            page_idx: 0,
            range: 0..3,
        },
        SearchMatch {
            page_idx: 0,
            range: 1..2,
        },
        SearchMatch {
            page_idx: 1,
            range: 3..4,
        },
        SearchMatch {
            page_idx: 1,
            range: 4..99,
        },
        SearchMatch {
            page_idx: 2,
            range: 0..1,
        },
    ];

    let saved_highlights = vec![Highlight {
        id: 1,
        path: PathBuf::from("/tmp/document.pdf"),
        color: HighlightColor::Yellow,
        rects: vec![
            HighlightRect {
                page_idx: 0,
                x0: 1.0,
                y0: 2.0,
                x1: 3.0,
                y1: 4.0,
            },
            HighlightRect {
                page_idx: 1,
                x0: 5.0,
                y0: 6.0,
                x1: 7.0,
                y1: 8.0,
            },
        ],
        semantic_range: None,
        text: String::new(),
        note: String::new(),
        created: 0,
    }];
    let notes = vec![Note {
        id: 2,
        path: PathBuf::from("/tmp/document.pdf"),
        color: HighlightColor::Blue,
        rects: vec![HighlightRect {
            page_idx: 1,
            x0: 9.0,
            y0: 10.0,
            x1: 11.0,
            y1: 12.0,
        }],
        semantic_range: None,
        text: String::new(),
        body: String::new(),
        created: 0,
    }];

    let page_zero = DocumentView::thumbnail_overlays_for_page_data(
        0,
        &saved_highlights,
        &notes,
        &glyphs,
        &matches,
        Some(0),
    );
    let page_one = DocumentView::thumbnail_overlays_for_page_data(
        1,
        &saved_highlights,
        &notes,
        &glyphs,
        &matches,
        Some(0),
    );

    assert_eq!(page_zero.highlights.len(), 1);
    assert!(page_zero.note_underlines.is_empty());
    assert_eq!(page_one.highlights.len(), 1);
    assert_eq!(page_one.note_underlines.len(), 1);
    assert_eq!(page_zero.search_highlights.len(), 3);
    assert_eq!(page_one.search_highlights.len(), 1);
    assert_eq!(
        page_zero.search_highlights[0].rect,
        HighlightRect {
            page_idx: 0,
            x0: 0.0,
            y0: 0.0,
            x1: 10.0,
            y1: 6.0,
        }
    );
    assert_eq!(
        page_zero
            .search_highlights
            .iter()
            .filter(|highlight| highlight.active)
            .count(),
        2
    );
    assert!(!page_zero.search_highlights[2].active);
    assert!(!page_one.search_highlights[0].active);
}

#[test]
fn thumbnail_search_bounds_follow_page_rotation_and_scale() {
    let page_bounds = Rect::new(10.0, 20.0, 110.0, 220.0);
    let rect = Rect::new(20.0, 40.0, 60.0, 100.0);
    let origin = point(px(7.0), px(11.0));

    for rotation in [
        DocumentRotation::Degrees0,
        DocumentRotation::Degrees90,
        DocumentRotation::Degrees180,
        DocumentRotation::Degrees270,
    ] {
        let page_dimensions = DocumentView::rotated_dimensions(100.0, 200.0, rotation);
        let image_bounds = Bounds::new(
            origin,
            size(px(page_dimensions.0 / 2.0), px(page_dimensions.1 / 2.0)),
        );
        let actual = DocumentView::pdf_rect_to_image_bounds(
            page_bounds,
            rotation,
            page_dimensions,
            rect,
            image_bounds,
        );
        let view_rect = DocumentView::pdf_rect_to_view(page_bounds, rotation, rect);

        assert!((actual.origin.x.as_f32() - (7.0 + view_rect.x0 / 2.0)).abs() < 0.001);
        assert!((actual.origin.y.as_f32() - (11.0 + view_rect.y0 / 2.0)).abs() < 0.001);
        assert!((actual.size.width.as_f32() - (view_rect.x1 - view_rect.x0) / 2.0).abs() < 0.001);
        assert!((actual.size.height.as_f32() - (view_rect.y1 - view_rect.y0) / 2.0).abs() < 0.001);
    }
}

#[test]
fn page_block_markdown_escapes_commonmark_and_preserves_spacing() {
    let blocks = vec![
        TextFormatBlock {
            lines: vec![
                "# Heading".to_string(),
                "literal *stars* and [link](url)".to_string(),
                String::new(),
            ],
            average_font_size: 12.0,
            bold_ratio: 0.0,
            layout: None,
        },
        TextFormatBlock {
            lines: vec!["next_block.".to_string()],
            average_font_size: 12.0,
            bold_ratio: 0.0,
            layout: None,
        },
    ];

    assert_eq!(
        DocumentView::format_block_markdown(&blocks),
        "\\# Heading\nliteral \\*stars\\* and \\[link\\]\\(url\\)\n\n\nnext\\_block\\."
    );
}

#[test]
fn semantic_page_markdown_converts_conservative_headings_and_lists() {
    let blocks = vec![
        TextFormatBlock {
            lines: vec!["Overview".to_string()],
            average_font_size: 24.0,
            bold_ratio: 0.9,
            layout: None,
        },
        TextFormatBlock {
            lines: vec!["plain *text*.".to_string()],
            average_font_size: 12.0,
            bold_ratio: 0.0,
            layout: None,
        },
        TextFormatBlock {
            lines: vec!["• First *item*".to_string(), "- Second".to_string()],
            average_font_size: 12.0,
            bold_ratio: 0.0,
            layout: None,
        },
        TextFormatBlock {
            lines: vec!["1) One".to_string(), "2. Two".to_string()],
            average_font_size: 12.0,
            bold_ratio: 0.0,
            layout: None,
        },
    ];

    assert_eq!(
        DocumentView::format_semantic_markdown(&blocks),
        "# Overview\n\nplain \\*text\\*\\.\n\n- First \\*item\\*\n- Second\n\n1. One\n2. Two"
    );
}

#[test]
fn selected_markdown_uses_glyph_boundaries_and_typography() {
    let mut glyphs = "Overview"
        .chars()
        .map(|c| test_glyph(c, 0.0, 6.0))
        .collect::<Vec<_>>();
    glyphs.extend("• First".chars().map(|c| {
        let mut glyph = test_glyph(c, 0.0, 6.0);
        glyph.block_idx = 1;
        glyph.line_idx = 1;
        glyph
    }));
    for glyph in &mut glyphs[..8] {
        glyph.font_size = 24.0;
        glyph.is_bold = true;
    }

    let blocks = DocumentView::text_blocks_for_glyph_range(&glyphs, 0..glyphs.len()).unwrap();

    assert_eq!(
        DocumentView::format_semantic_markdown(&blocks),
        "# Overview\n\n- First"
    );
}

#[test]
fn semantic_copy_joins_wrapped_lines_for_plain_text_and_markdown() {
    let mut glyphs = Vec::new();
    for (line_idx, text) in ["Two lines in a PDF document", "can be copied as one line."]
        .into_iter()
        .enumerate()
    {
        glyphs.extend(text.chars().map(|c| {
            let mut glyph = test_glyph(c, 0.0, 6.0);
            glyph.line_idx = line_idx as u32;
            glyph
        }));
    }
    let semantic =
        DocumentView::copy_blocks_for_glyph_range(&glyphs, 0..glyphs.len(), true).unwrap();
    let physical =
        DocumentView::copy_blocks_for_glyph_range(&glyphs, 0..glyphs.len(), false).unwrap();

    assert_eq!(
        DocumentView::plain_text_for_blocks(&semantic).as_deref(),
        Some("Two lines in a PDF document can be copied as one line.")
    );
    assert_eq!(
        DocumentView::format_semantic_markdown(&semantic),
        "Two lines in a PDF document can be copied as one line\\."
    );
    assert_eq!(
        DocumentView::plain_text_for_blocks(&physical).as_deref(),
        Some("Two lines in a PDF document\ncan be copied as one line.")
    );
    assert_eq!(
        DocumentView::format_semantic_markdown(&physical),
        "Two lines in a PDF document\ncan be copied as one line\\."
    );
}

#[test]
fn semantic_copy_joins_continuous_pdf_text_blocks() {
    let mut glyphs = Vec::new();
    for (block_idx, line_idx, text, line_rect) in [
        (
            0,
            0,
            "▪Message passing",
            Rect::new(442.0, 149.0, 632.0, 172.0),
        ),
        (1, 1, "between nodes", Rect::new(464.0, 175.0, 616.0, 193.0)),
    ] {
        glyphs.extend(text.chars().map(|c| {
            let mut glyph = test_glyph(c, line_rect.x0, line_rect.x1);
            glyph.block_idx = block_idx;
            glyph.line_idx = line_idx;
            glyph.line_rect = line_rect;
            glyph
        }));
    }

    let blocks = DocumentView::copy_blocks_for_glyph_range(&glyphs, 0..glyphs.len(), true).unwrap();

    assert_eq!(
        DocumentView::plain_text_for_blocks(&blocks).as_deref(),
        Some("▪Message passing between nodes")
    );
    assert_eq!(blocks.len(), 1);
}

#[test]
fn semantic_copy_keeps_parallel_columns_separate() {
    let mut glyphs = Vec::new();
    for (block_idx, text, line_rect) in [
        (0, "Shared memory", Rect::new(35.0, 149.0, 350.0, 172.0)),
        (
            1,
            "Distributed memory",
            Rect::new(442.0, 149.0, 632.0, 172.0),
        ),
    ] {
        glyphs.extend(text.chars().map(|c| {
            let mut glyph = test_glyph(c, line_rect.x0, line_rect.x1);
            glyph.block_idx = block_idx;
            glyph.line_idx = block_idx;
            glyph.line_rect = line_rect;
            glyph
        }));
    }

    let blocks = DocumentView::copy_blocks_for_glyph_range(&glyphs, 0..glyphs.len(), true).unwrap();

    assert_eq!(
        DocumentView::plain_text_for_blocks(&blocks).as_deref(),
        Some("Shared memory\n\nDistributed memory")
    );
    assert_eq!(blocks.len(), 2);
}

#[test]
fn semantic_copy_dehyphenates_word_continuations_and_preserves_list_items() {
    assert_eq!(
        DocumentView::semantic_logical_lines(&[
            "context-aware dehyphen-".to_string(),
            "ation".to_string(),
            "• First item".to_string(),
            "continuation".to_string(),
            "• Second item".to_string(),
        ]),
        vec![
            "context-aware dehyphenation",
            "• First item continuation",
            "• Second item",
        ]
    );
    assert_eq!(
        DocumentView::semantic_logical_lines(&["-".to_string(), "value".to_string()]),
        vec!["- value"]
    );
}

#[test]
fn semantic_copy_preserves_text_block_boundaries() {
    let blocks = vec![
        TextFormatBlock {
            lines: vec!["left block".to_string()],
            average_font_size: 12.0,
            bold_ratio: 0.0,
            layout: None,
        },
        TextFormatBlock {
            lines: vec!["right block".to_string()],
            average_font_size: 12.0,
            bold_ratio: 0.0,
            layout: None,
        },
    ];

    assert_eq!(
        DocumentView::plain_text_for_blocks(&blocks).as_deref(),
        Some("left block\n\nright block")
    );
}

#[test]
fn plain_text_markdown_escapes_translated_text() {
    assert_eq!(
        DocumentView::markdown_for_plain_text("literal *text*\n\nnext"),
        "literal \\*text\\*\n\nnext"
    );
}

#[test]
fn page_image_dimensions_preserve_aspect_ratio_at_preset_edge() {
    assert_eq!(
        DocumentView::page_image_dimensions(612.0, 792.0, 1600).unwrap(),
        (1236, 1600)
    );
    assert_eq!(
        DocumentView::page_image_dimensions(792.0, 612.0, 1024).unwrap(),
        (1024, 791)
    );
    assert!(DocumentView::page_image_dimensions(0.0, 612.0, 1024).is_err());
}

#[test]
fn render_page_jpeg_encodes_rgb_pixmap() {
    let page_bounds = Rect::new(0.0, 0.0, 10.0, 20.0);
    let display_list = mupdf::DisplayList::new(page_bounds).unwrap();

    let bytes = DocumentView::render_page_jpeg(
        &display_list,
        page_bounds,
        page_bounds.width(),
        page_bounds.height(),
        crate::settings::PageImageQuality::Low,
        DocumentRotation::Degrees0,
    )
    .unwrap();
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).unwrap();

    assert_eq!(image.dimensions(), (512, 1024));
}

#[test]
fn rotated_page_jpeg_swaps_image_dimensions() {
    let page_bounds = Rect::new(0.0, 0.0, 10.0, 20.0);
    let display_list = mupdf::DisplayList::new(page_bounds).unwrap();

    let bytes = DocumentView::render_page_jpeg(
        &display_list,
        page_bounds,
        page_bounds.width(),
        page_bounds.height(),
        crate::settings::PageImageQuality::Low,
        DocumentRotation::Degrees90,
    )
    .unwrap();
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).unwrap();

    assert_eq!(image.dimensions(), (1024, 512));
}

#[test]
fn rotation_point_and_rect_transforms_round_trip() {
    let page_bounds = Rect::new(10.0, 20.0, 110.0, 220.0);
    let point = mupdf::Point { x: 35.0, y: 70.0 };
    let rect = Rect::new(20.0, 40.0, 60.0, 100.0);

    for rotation in [
        DocumentRotation::Degrees0,
        DocumentRotation::Degrees90,
        DocumentRotation::Degrees180,
        DocumentRotation::Degrees270,
    ] {
        let view_point = DocumentView::pdf_point_to_view(page_bounds, rotation, point);
        let restored = DocumentView::view_point_to_pdf(page_bounds, rotation, view_point);
        assert!((restored.x - point.x).abs() < 0.001);
        assert!((restored.y - point.y).abs() < 0.001);

        let view_rect = DocumentView::pdf_rect_to_view(page_bounds, rotation, rect);
        let restored = DocumentView::view_rect_to_pdf(page_bounds, rotation, view_rect);
        assert!((restored.x0 - rect.x0).abs() < 0.001);
        assert!((restored.y0 - rect.y0).abs() < 0.001);
        assert!((restored.x1 - rect.x1).abs() < 0.001);
        assert!((restored.y1 - rect.y1).abs() < 0.001);
    }
}

#[test]
fn rotation_render_matrix_matches_view_transform() {
    let page_bounds = Rect::new(10.0, 20.0, 110.0, 220.0);
    let point = mupdf::Point { x: 35.0, y: 70.0 };
    let view_origin = mupdf::Point { x: 5.0, y: 7.0 };

    for rotation in [
        DocumentRotation::Degrees0,
        DocumentRotation::Degrees90,
        DocumentRotation::Degrees180,
        DocumentRotation::Degrees270,
    ] {
        let matrix = DocumentView::render_matrix(page_bounds, rotation, 2.0, 3.0, view_origin);
        let transformed = matrix.transform_xy(point.x, point.y);
        let view_point = DocumentView::pdf_point_to_view(page_bounds, rotation, point);
        assert!((transformed.0 - (view_point.x - view_origin.x) * 2.0).abs() < 0.001);
        assert!((transformed.1 - (view_point.y - view_origin.y) * 3.0).abs() < 0.001);
    }
}

fn test_render_image() -> Arc<gpui::RenderImage> {
    Arc::new(gpui::RenderImage::new(smallvec![Frame::new(
        RgbaImage::new(1, 1),
    )]))
}

#[test]
fn translation_panel_uses_pane_local_width_and_default_right_edge() {
    let wide_width = translation_panel_width(1_000.0);
    let wide = default_translation_panel_position(1_000.0, wide_width);
    assert_eq!(wide_width, TRANSLATION_PANEL_PREFERRED_WIDTH);
    assert_eq!(wide.x.as_f32(), 594.0);
    assert_eq!(wide.y.as_f32(), TRANSLATION_PANEL_DEFAULT_TOP);

    let narrow_width = translation_panel_width(240.0);
    let narrow = default_translation_panel_position(240.0, narrow_width);
    assert_eq!(narrow_width, 216.0);
    assert_eq!(narrow.x.as_f32(), TRANSLATION_PANEL_MARGIN);
    assert!(narrow.x.as_f32() + narrow_width <= 240.0 - TRANSLATION_PANEL_DEFAULT_RIGHT);
}

#[test]
fn translation_affordances_are_scoped_to_the_active_live_pair() {
    assert!(!translation_pair_highlights_enabled(
        TranslationViewRole::None,
        true
    ));
    assert!(!translation_pair_highlights_enabled(
        TranslationViewRole::Translation,
        false
    ));
    assert!(translation_pair_highlights_enabled(
        TranslationViewRole::Translation,
        true
    ));
    assert!(translation_pair_highlights_enabled(
        TranslationViewRole::SourceMirror,
        true
    ));

    assert!(translation_toolbar_enabled(
        TranslationViewRole::Translation,
        true
    ));
    assert!(!translation_toolbar_enabled(
        TranslationViewRole::SourceMirror,
        true
    ));
    assert!(!translation_toolbar_enabled(
        TranslationViewRole::None,
        true
    ));
}

#[test]
fn translation_panel_drag_is_reclamped_after_pane_resize() {
    let resized_width = translation_panel_width(300.0);
    let clamped = clamp_translation_panel_position(
        point(px(700.0), px(900.0)),
        300.0,
        180.0,
        size(px(resized_width), px(48.0)),
    );
    assert_eq!(clamped.x.as_f32(), 16.0);
    assert_eq!(clamped.y.as_f32(), 124.0);

    let tiny_width = translation_panel_width(72.0);
    let tiny = clamp_translation_panel_position(
        point(px(-20.0), px(-20.0)),
        72.0,
        40.0,
        size(px(tiny_width), px(40.0)),
    );
    assert_eq!(tiny.x.as_f32(), TRANSLATION_PANEL_MARGIN);
    assert_eq!(tiny.y.as_f32(), TRANSLATION_PANEL_MARGIN);
}

#[test]
fn translation_panel_layout_stays_bounded_at_reported_pane_widths() {
    assert_eq!(translation_panel_width(590.0), 390.0);
    assert_eq!(translation_panel_width(320.0), 296.0);
    assert_eq!(translation_panel_width(220.0), 196.0);
    assert!(translation_panel_width(590.0) >= 300.0);
    assert!(translation_panel_width(320.0) < 300.0);
    assert!(translation_panel_width(220.0) < 300.0);
}

#[test]
fn translation_panel_snaps_only_inside_the_edge_magnet_zones() {
    let viewport_width = 600.0;
    let panel_width = 200.0;
    assert_eq!(
        translation_panel_snap_side(
            point(px(TRANSLATION_PANEL_SNAP_ZONE), px(40.0)),
            panel_width,
            viewport_width,
        ),
        Some(TranslationPanelSide::Left)
    );
    assert_eq!(
        translation_panel_snap_side(point(px(200.0), px(40.0)), panel_width, viewport_width,),
        None
    );
    assert_eq!(
        translation_panel_snap_side(
            point(
                px(viewport_width - panel_width - TRANSLATION_PANEL_SNAP_ZONE),
                px(40.0),
            ),
            panel_width,
            viewport_width,
        ),
        Some(TranslationPanelSide::Right)
    );
    assert_eq!(
        clamp_translation_panel_edge_y(px(900.0), 180.0).as_f32(),
        132.0
    );
}

#[test]
fn translation_panel_drawer_geometry_is_symmetric_and_delayed() {
    let viewport_width = 600.0;
    let panel_width = 200.0;
    assert_eq!(
        translation_panel_docked_x(TranslationPanelSide::Left, 0.0, viewport_width, panel_width,)
            .as_f32(),
        -panel_width
    );
    assert_eq!(
        translation_panel_docked_x(TranslationPanelSide::Left, 1.0, viewport_width, panel_width,)
            .as_f32(),
        TRANSLATION_PANEL_MARGIN
    );
    assert_eq!(
        translation_panel_docked_x(
            TranslationPanelSide::Right,
            0.0,
            viewport_width,
            panel_width,
        )
        .as_f32(),
        viewport_width
    );
    assert_eq!(
        translation_panel_docked_x(
            TranslationPanelSide::Right,
            1.0,
            viewport_width,
            panel_width,
        )
        .as_f32(),
        viewport_width - panel_width - TRANSLATION_PANEL_MARGIN
    );
    assert_eq!(TRANSLATION_PANEL_DOCK_DELAY, Duration::from_millis(1500));
    assert_eq!(TRANSLATION_PANEL_RETRACT_DELAY, Duration::from_millis(400));
}

#[test]
fn root_notifications_are_above_translation_overlays() {
    assert!(glyphst_ui::root::ROOT_NOTIFICATION_PRIORITY > TRANSLATION_PANEL_OVERLAY_PRIORITY);
    assert!(
        glyphst_ui::root::ROOT_NOTIFICATION_PRIORITY > TRANSLATION_HOVER_TOOLS_OVERLAY_PRIORITY
    );
    assert!(TRANSLATION_PANEL_DRAG_OVERLAY_PRIORITY > glyphst_ui::root::ROOT_NOTIFICATION_PRIORITY);
}

#[test]
fn translation_hover_animation_retargets_after_becoming_invisible() {
    let large = Bounds::new(point(px(20.0), px(30.0)), size(px(240.0), px(120.0)));
    let small = Bounds::new(point(px(24.0), px(156.0)), size(px(90.0), px(24.0)));
    let mut animation = TranslationHoverAnimation::new("large".to_string(), large);
    let now = Instant::now();

    animation.fade_out(now, false);
    assert_eq!(animation.opacity.target(), 0.0);
    animation.retarget("small".to_string(), small, now, false);

    assert_eq!(animation.region_id, "small");
    assert_eq!(animation.opacity.target(), 1.0);
    assert_eq!(animation.x.target(), small.origin.x.as_f32());
    assert_eq!(animation.y.target(), small.origin.y.as_f32());
    assert_eq!(animation.width.target(), small.size.width.as_f32());
    assert_eq!(animation.height.target(), small.size.height.as_f32());
    assert_eq!(animation.bounds(now), large);
}

#[test]
fn render_cache_protects_only_the_exact_visible_render() {
    let unrotated = RenderKey {
        doc_id: 1,
        page_idx: 0,
        source_revision: 0,
        rotation: DocumentRotation::Degrees0,
        physical_width: 8192,
        physical_height: 8192,
    };
    let rotated = RenderKey {
        rotation: DocumentRotation::Degrees90,
        ..unrotated
    };
    let mut cache = RenderCache::new();
    cache.set_protected_renders(1, [rotated]);

    cache.insert_render(unrotated, 1.0, test_render_image());
    cache.insert_render(rotated, 1.0, test_render_image());

    assert!(cache.render(unrotated).is_none());
    assert!(cache.render(rotated).is_some());
}

#[test]
fn render_cache_resize_churn_keeps_current_viewport_and_thumbnail_sizes() {
    let key = |physical_width, physical_height| RenderKey {
        doc_id: 1,
        page_idx: 0,
        source_revision: 0,
        rotation: DocumentRotation::Degrees0,
        physical_width,
        physical_height,
    };
    let half_width = key(4_000, 3_000);
    let fullscreen = key(6_000, 4_000);
    let current_viewport = key(5_000, 4_000);
    let visible_thumbnail = key(260, 360);
    let mut cache = RenderCache::new();
    cache.set_protected_renders(1, [current_viewport, visible_thumbnail]);

    cache.insert_render(half_width, 1.0, test_render_image());
    cache.insert_render(fullscreen, 1.0, test_render_image());
    cache.insert_render(current_viewport, 1.0, test_render_image());
    cache.insert_render(visible_thumbnail, 1.0, test_render_image());

    assert!(cache.render(half_width).is_none());
    assert!(cache.render(fullscreen).is_none());
    assert!(cache.render(current_viewport).is_some());
    assert!(cache.render(visible_thumbnail).is_some());
}

#[test]
fn dock_motion_reuses_a_fallback_then_requests_the_settled_size() {
    assert!(!should_start_viewport_render(false, false, true));
    assert!(should_start_viewport_render(false, true, true));
    assert!(should_start_viewport_render(false, false, false));
    assert!(!should_start_viewport_render(true, true, true));
}

#[test]
fn render_cache_reports_and_protects_the_texture_used_during_resize() {
    let fallback = RenderKey {
        doc_id: 1,
        page_idx: 0,
        source_revision: 0,
        rotation: DocumentRotation::Degrees0,
        physical_width: 5_000,
        physical_height: 4_000,
    };
    let settled = RenderKey {
        physical_width: 6_000,
        ..fallback
    };
    let mut cache = RenderCache::new();
    cache.insert_render(fallback, 1.0, test_render_image());

    let (resolved, _) = cache
        .scaled_render_with_key(1, 0, DocumentRotation::Degrees0, 1.2)
        .expect("the previous viewport raster should be reusable");
    assert!(resolved == fallback);

    cache.set_protected_renders(1, [fallback, settled]);
    cache.insert_render(settled, 1.2, test_render_image());
    assert!(cache.render(fallback).is_some());
    assert!(cache.render(settled).is_some());
}

#[test]
fn surface_render_keys_keep_actual_fractional_scale_bounds_and_prune_hidden_pages() {
    // A 168 x 217.411765 logical thumbnail at 1.5x predicts 252 x 327
    // when dimensions are rounded independently. GPUI snaps the absolute
    // canvas edges, so its actual height can instead be 326 physical pixels.
    let predicted = RenderKey {
        doc_id: 1,
        page_idx: 0,
        source_revision: 0,
        rotation: DocumentRotation::Degrees0,
        physical_width: 252,
        physical_height: 327,
    };
    let actual = RenderKey {
        physical_height: 326,
        ..predicted
    };
    let resized_actual = RenderKey {
        physical_width: 253,
        ..actual
    };
    let next_page = RenderKey {
        page_idx: 1,
        ..actual
    };
    let mut keys = SurfaceRenderKeys::default();

    assert!(predicted != actual);
    assert!(keys.register(0, actual));
    assert!(!keys.register(0, actual));
    assert!(keys.register(0, resized_actual));
    assert!(keys.register(1, next_page));

    let registered = keys.values().collect::<HashSet<_>>();
    assert!(!registered.contains(&predicted));
    assert!(!registered.contains(&actual));
    assert!(registered.contains(&resized_actual));
    assert!(registered.contains(&next_page));

    assert!(keys.retain_pages([1]));
    let retained = keys.values().collect::<HashSet<_>>();
    assert_eq!(retained.len(), 1);
    assert!(retained.contains(&next_page));
    assert!(!keys.retain_pages([1]));
}

#[test]
fn render_cache_page_boundary_keeps_actual_visible_thumbnail() {
    let thumbnail = RenderKey {
        doc_id: 1,
        page_idx: 0,
        source_revision: 0,
        rotation: DocumentRotation::Degrees0,
        physical_width: 252,
        physical_height: 326,
    };
    let fullscreen_page = |page_idx| RenderKey {
        doc_id: 1,
        page_idx,
        source_revision: 0,
        rotation: DocumentRotation::Degrees0,
        physical_width: 2_770,
        physical_height: 3_585,
    };
    let old_page = fullscreen_page(0);
    let middle_page = fullscreen_page(1);
    let current_page = fullscreen_page(2);
    let entering_page = fullscreen_page(3);
    let mut surface_keys = SurfaceRenderKeys::default();
    surface_keys.register(0, thumbnail);

    let mut cache = RenderCache::new();
    cache.set_protected_renders(
        1,
        surface_keys.values().chain(std::iter::once(entering_page)),
    );
    cache.insert_render(thumbnail, 1.0, test_render_image());
    cache.insert_render(old_page, 1.0, test_render_image());
    cache.insert_render(middle_page, 1.0, test_render_image());
    cache.insert_render(current_page, 1.0, test_render_image());

    // Crossing into the next page pushes the cache beyond 30M pixels.
    // The stale high-resolution page must be evicted, not the thumbnail.
    cache.insert_render(entering_page, 1.0, test_render_image());

    assert!(cache.render(old_page).is_none());
    assert!(cache.render(thumbnail).is_some());
    assert!(cache.render(entering_page).is_some());
}

#[test]
fn render_cache_protection_is_independent_per_document_and_cleared_on_eviction() {
    let document_one = RenderKey {
        doc_id: 1,
        page_idx: 0,
        source_revision: 0,
        rotation: DocumentRotation::Degrees0,
        physical_width: 5_000,
        physical_height: 4_000,
    };
    let document_two = RenderKey {
        doc_id: 2,
        ..document_one
    };
    let document_two_thumbnail = RenderKey {
        physical_width: 260,
        physical_height: 360,
        ..document_two
    };
    let mut cache = RenderCache::new();
    cache.set_protected_renders(1, [document_one]);
    cache.set_protected_renders(2, [document_two]);

    cache.insert_render(document_one, 1.0, test_render_image());
    cache.insert_render(document_two, 1.0, test_render_image());
    assert!(cache.render(document_one).is_some());
    assert!(cache.render(document_two).is_some());

    cache.clear_protected_renders(1);
    cache.set_protected_renders(2, [document_two, document_two_thumbnail]);
    cache.insert_render(document_two_thumbnail, 1.0, test_render_image());
    assert!(cache.render(document_one).is_none());
    assert!(cache.render(document_two).is_some());
    assert!(cache.render(document_two_thumbnail).is_some());

    cache.evict_document(2);
    assert!(!cache.is_render_protected(document_two));
}

#[test]
fn region_drag_intersects_centered_pages_and_omits_gaps() {
    let pages = vec![
        DocumentPage {
            bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
            width: 100.0,
            height: 100.0,
            offset_y: 0.0,
            glyph_range: 0..0,
        },
        DocumentPage {
            bounds: Rect::new(10.0, 20.0, 70.0, 120.0),
            width: 60.0,
            height: 100.0,
            offset_y: 120.0,
            glyph_range: 0..0,
        },
    ];
    let reference = DocumentView::region_reference_for_pages(
        &pages,
        100.0,
        1.0,
        DocumentRotation::Degrees0,
        RegionDragState {
            anchor: Point::new(10.0, 50.0),
            current: Point::new(90.0, 170.0),
        },
    )
    .unwrap();

    assert_eq!(
        reference.regions,
        vec![
            PageRegion {
                page: PageIndex(0),
                x0: 10.0,
                y0: 50.0,
                x1: 90.0,
                y1: 100.0,
            },
            PageRegion {
                page: PageIndex(1),
                x0: 10.0,
                y0: 20.0,
                x1: 70.0,
                y1: 70.0,
            },
        ]
    );
}

#[test]
fn region_drag_rejects_tiny_marquees() {
    let pages = [DocumentPage {
        bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
        width: 100.0,
        height: 100.0,
        offset_y: 0.0,
        glyph_range: 0..0,
    }];
    assert!(
        DocumentView::region_reference_for_pages(
            &pages,
            100.0,
            1.0,
            DocumentRotation::Degrees0,
            RegionDragState {
                anchor: Point::new(10.0, 10.0),
                current: Point::new(12.0, 30.0),
            },
        )
        .is_none()
    );
}

#[test]
fn rotated_region_drag_maps_back_to_pdf_coordinates() {
    let pages = [DocumentPage {
        bounds: Rect::new(10.0, 20.0, 110.0, 220.0),
        width: 100.0,
        height: 200.0,
        offset_y: 0.0,
        glyph_range: 0..0,
    }];
    let reference = DocumentView::region_reference_for_pages(
        &pages,
        200.0,
        1.0,
        DocumentRotation::Degrees90,
        RegionDragState {
            anchor: Point::new(20.0, 10.0),
            current: Point::new(80.0, 50.0),
        },
    )
    .unwrap();

    assert_eq!(
        reference.regions,
        vec![PageRegion {
            page: PageIndex(0),
            x0: 20.0,
            y0: 140.0,
            x1: 60.0,
            y1: 200.0,
        }]
    );
}

#[test]
fn successful_region_copy_clears_the_copied_selection() {
    let copied = RegionImageReference::new([PageRegion {
        page: PageIndex(0),
        x0: 10.0,
        y0: 20.0,
        x1: 30.0,
        y1: 40.0,
    }]);
    let mut selected_region = Some(copied.clone());
    let mut context_menu_region = true;

    assert!(clear_copied_region_selection(
        &mut selected_region,
        &mut context_menu_region,
        &copied,
    ));
    assert!(selected_region.is_none());
    assert!(!context_menu_region);
}

#[test]
fn completed_region_copy_preserves_a_newer_selection() {
    let copied = RegionImageReference::new([PageRegion {
        page: PageIndex(0),
        x0: 10.0,
        y0: 20.0,
        x1: 30.0,
        y1: 40.0,
    }]);
    let newer = RegionImageReference::new([PageRegion {
        page: PageIndex(1),
        x0: 50.0,
        y0: 60.0,
        x1: 70.0,
        y1: 80.0,
    }]);
    let mut selected_region = Some(newer.clone());
    let mut context_menu_region = true;

    assert!(!clear_copied_region_selection(
        &mut selected_region,
        &mut context_menu_region,
        &copied,
    ));
    assert_eq!(selected_region, Some(newer));
    assert!(context_menu_region);
}

#[test]
fn region_compositor_stacks_crops_and_centers_narrow_pages() {
    let page_bounds = Rect::new(0.0, 0.0, 100.0, 100.0);
    let display_list = Arc::new(mupdf::DisplayList::new(page_bounds).unwrap());
    let exports = vec![
        RegionImageExport {
            display_list: display_list.clone(),
            page_bounds,
            region: PageRegion {
                page: PageIndex(0),
                x0: 0.0,
                y0: 0.0,
                x1: 10.0,
                y1: 20.0,
            },
            rotation: DocumentRotation::Degrees0,
        },
        RegionImageExport {
            display_list,
            page_bounds,
            region: PageRegion {
                page: PageIndex(1),
                x0: 0.0,
                y0: 0.0,
                x1: 20.0,
                y1: 10.0,
            },
            rotation: DocumentRotation::Degrees0,
        },
    ];

    let (width, height, rgba) =
        DocumentView::rasterize_region_stack_to_rgba(&exports, 2.0).unwrap();
    assert_eq!((width, height), (40, 60));
    assert_eq!(rgba.len(), 40 * 60 * 4);
}

#[test]
fn region_compositor_rejects_oversized_exports_before_rendering() {
    let page_bounds = Rect::new(0.0, 0.0, 10_000.0, 5_000.0);
    let display_list = Arc::new(mupdf::DisplayList::new(page_bounds).unwrap());
    let exports = [RegionImageExport {
        display_list,
        page_bounds,
        region: PageRegion {
            page: PageIndex(0),
            x0: 0.0,
            y0: 0.0,
            x1: 10_000.0,
            y1: 5_000.0,
        },
        rotation: DocumentRotation::Degrees0,
    }];

    assert!(DocumentView::rasterize_region_stack_to_rgba(&exports, 1.0).is_err());
}

#[test]
fn rotated_region_compositor_swaps_crop_dimensions() {
    let page_bounds = Rect::new(0.0, 0.0, 100.0, 100.0);
    let exports = [RegionImageExport {
        display_list: Arc::new(mupdf::DisplayList::new(page_bounds).unwrap()),
        page_bounds,
        region: PageRegion {
            page: PageIndex(0),
            x0: 0.0,
            y0: 0.0,
            x1: 10.0,
            y1: 20.0,
        },
        rotation: DocumentRotation::Degrees90,
    }];

    let (width, height, _) = DocumentView::rasterize_region_stack_to_rgba(&exports, 2.0).unwrap();
    assert_eq!((width, height), (40, 20));
}

#[test]
fn page_reference_validation_rejects_invalid_pages_and_deduplicates_exact_refs() {
    let page = MentionReference::new(MentionKind::Page, Some(2));
    let image = MentionReference::new(MentionKind::PageImage, Some(2));
    let selected_text = MentionReference::new(MentionKind::SelectedPagesText, Some(1));
    let selected_image = MentionReference::new(MentionKind::SelectedPagesImage, Some(3));
    let document_image = MentionReference::new(MentionKind::DocumentImage, None);
    let references = DocumentView::prepare_page_references(
        vec![
            MentionReference::new(MentionKind::Selection, None),
            page,
            image,
            selected_text,
            selected_image,
            document_image,
            page,
            document_image,
        ],
        DocumentLayout::Fixed,
        3,
    )
    .unwrap();
    assert_eq!(
        references,
        vec![
            page,
            image,
            selected_text,
            selected_image,
            MentionReference::new(MentionKind::DocumentImage, Some(1)),
            MentionReference::new(MentionKind::DocumentImage, Some(2)),
            MentionReference::new(MentionKind::DocumentImage, Some(3)),
        ]
    );

    assert!(
        DocumentView::prepare_page_references(
            vec![MentionReference::new(MentionKind::Page, None)],
            DocumentLayout::Fixed,
            3,
        )
        .unwrap_err()
        .to_string()
        .contains("requires a 1-based page number")
    );
    for invalid_page in [0, 4] {
        assert!(
            DocumentView::prepare_page_references(
                vec![MentionReference::new(
                    MentionKind::PageImage,
                    Some(invalid_page),
                )],
                DocumentLayout::Fixed,
                3,
            )
            .unwrap_err()
            .to_string()
            .contains("page must be between 1 and 3")
        );
    }
    let paged_document_image = MentionReference::new(MentionKind::DocumentImage, Some(1));
    assert!(
        DocumentView::prepare_page_references(vec![paged_document_image], DocumentLayout::Fixed, 3,)
            .unwrap_err()
            .to_string()
            .contains("does not accept a page number")
    );
    assert!(
        DocumentView::prepare_page_references(
            vec![document_image],
            DocumentLayout::Reflow(EpubLayout::default()),
            3,
        )
        .unwrap_err()
        .to_string()
        .contains("reflowable documents")
    );
}

#[test]
fn provisional_reflow_pages_use_uniform_sample_bounds() {
    let loaded =
        DocumentView::provisional_reflow_pages(3, Rect::new(0.0, 0.0, 320.0, 480.0), 12.0).unwrap();
    assert_eq!(loaded.pages.len(), 3);
    assert_eq!(loaded.pages[0].offset_y, 0.0);
    assert_eq!(loaded.pages[1].offset_y, 492.0);
    assert_eq!(loaded.pages[2].offset_y, 984.0);
    assert_eq!(loaded.content_height_doc, 1464.0);
}

fn test_glyph(c: char, x0: f32, x1: f32) -> Glyph {
    Glyph {
        c,
        quad: Quad::new(
            Point::new(x0, 0.0),
            Point::new(x1, 0.0),
            Point::new(x0, 6.0),
            Point::new(x1, 6.0),
        ),
        line_rect: Rect::new(x0, 0.0, x1, 6.0),
        page_idx: 0,
        line_idx: 0,
        block_idx: 0,
        font_size: 12.0,
        is_bold: false,
        inferred_space_after: false,
    }
}

#[test]
fn wheel_zoom_uses_current_event_modifiers() {
    let plain = ScrollWheelEvent::default();
    assert!(!DocumentView::scroll_wheel_requests_zoom(&plain));

    let zoom = ScrollWheelEvent {
        modifiers: Modifiers {
            control: true,
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(DocumentView::scroll_wheel_requests_zoom(&zoom));
}

#[test]
fn document_replacement_clears_popup_and_preserves_pinned_mode() {
    let mut popup = Some(TranslationCardState::new(
        "popup",
        TranslationStatus::Loading,
        Some(point(px(10.0), px(20.0))),
        7,
    ));
    let mut pinned = Some(TranslationCardState::new(
        "pinned",
        TranslationStatus::Loading,
        None,
        7,
    ));

    reset_translation_cards_for_document_replacement(&mut popup, &mut pinned, 8);

    assert!(popup.is_none());
    let pinned = pinned.unwrap();
    assert!(pinned.source.is_empty());
    assert!(matches!(pinned.status, TranslationStatus::Idle));
    assert!(pinned.position.is_none());
    assert_eq!(pinned.request_generation, 8);
}

#[test]
fn document_replacement_does_not_enable_pinned_mode() {
    let mut popup = None;
    let mut pinned = None;

    reset_translation_cards_for_document_replacement(&mut popup, &mut pinned, 8);

    assert!(popup.is_none());
    assert!(pinned.is_none());
}

#[test]
fn document_translation_default_output_uses_target_language() {
    let target = LanguageCode::new("ZH-Hans").unwrap();

    assert_eq!(
        DocumentView::document_translation_output_path(
            std::path::Path::new("/tmp/report.v1.pdf"),
            &target,
        ),
        std::path::PathBuf::from("/tmp/report.v1.zh-hans.pdf")
    );
}

fn missing_space_glyphs() -> Vec<Glyph> {
    let mut glyphs = [
        ('1', 0.0, 5.0),
        ('0', 5.0, 10.0),
        ('0', 10.0, 15.0),
        ('b', 16.6, 21.6),
        ('y', 21.6, 26.6),
        ('d', 28.2, 33.2),
        ('e', 33.2, 38.2),
        ('f', 38.2, 43.2),
        ('a', 43.2, 48.2),
        ('u', 48.2, 53.2),
        ('l', 53.2, 58.2),
        ('t', 58.2, 63.2),
    ]
    .into_iter()
    .map(|(c, x0, x1)| test_glyph(c, x0, x1))
    .collect::<Vec<_>>();
    DocumentView::mark_inferred_spaces(&mut glyphs);
    glyphs
}

#[test]
fn glyph_line_rect_uses_nominal_glyph_height() {
    let mut glyph = test_glyph('x', 2.0, 7.0);
    glyph.quad = Quad::new(
        Point::new(2.0, 2.0),
        Point::new(7.0, 2.0),
        Point::new(2.0, 4.0),
        Point::new(7.0, 4.0),
    );
    glyph.line_rect = Rect::new(0.0, 0.0, 10.0, 8.0);

    assert_eq!(
        DocumentView::glyph_line_rect(&glyph),
        Rect::new(2.0, 2.0, 7.0, 4.0)
    );
}

#[test]
fn stored_annotation_height_is_normalized_to_matching_glyphs() {
    let mut glyphs = vec![test_glyph('A', 0.0, 5.0), test_glyph('g', 5.0, 10.0)];
    glyphs[0].line_rect = Rect::new(0.0, 0.0, 10.0, 6.0);
    glyphs[1].line_rect = glyphs[0].line_rect;
    let lines = vec![TextLine {
        glyph_range: 0..2,
        bounds: glyphs[0].line_rect,
    }];
    let stored = HighlightRect {
        page_idx: 0,
        x0: 0.0,
        y0: 2.0,
        x1: 10.0,
        y1: 4.0,
    };

    assert_eq!(
        DocumentView::normalize_stored_annotation_rect(&glyphs, &lines, stored),
        HighlightRect {
            y0: 0.0,
            y1: 6.0,
            ..stored
        }
    );
}

#[test]
fn dotted_underlines_keep_a_fixed_cadence() {
    let bounds = Bounds::new(point(px(10.0), px(20.0)), size(px(14.0), px(12.0)));
    let segments = DocumentView::dotted_underline_segments(bounds, px(3.0));

    assert_eq!(segments.len(), 3);
    assert_eq!(segments[0].origin, point(px(10.0), px(28.0)));
    assert_eq!(segments[1].origin.x, px(16.0));
    assert_eq!(segments[2].origin.x, px(22.0));
    assert_eq!(segments[2].size.width, px(2.0));
}

#[test]
fn nearest_line_uses_horizontal_distance_for_multi_column_pages() {
    let lines = vec![
        TextLine {
            glyph_range: 0..10,
            bounds: Rect::new(35.0, 148.0, 350.0, 172.0),
        },
        TextLine {
            glyph_range: 10..20,
            bounds: Rect::new(442.0, 149.0, 632.0, 172.0),
        },
    ];

    let right_text = DocumentView::nearest_line_in_slice(&lines, Point::new(600.0, 180.0)).unwrap();
    let right_gutter =
        DocumentView::nearest_line_in_slice(&lines, Point::new(410.0, 180.0)).unwrap();

    assert_eq!(right_text.1, 10);
    assert_eq!(right_gutter.1, 10);
}

#[test]
fn nearest_line_is_stable_for_reverse_multi_column_drags() {
    let lines = vec![
        TextLine {
            glyph_range: 0..10,
            bounds: Rect::new(35.0, 148.0, 350.0, 172.0),
        },
        TextLine {
            glyph_range: 10..20,
            bounds: Rect::new(442.0, 149.0, 632.0, 172.0),
        },
        TextLine {
            glyph_range: 20..30,
            bounds: Rect::new(464.0, 175.0, 616.0, 193.0),
        },
    ];

    let lower = DocumentView::nearest_line_in_slice(&lines, Point::new(610.0, 198.0)).unwrap();
    let upper = DocumentView::nearest_line_in_slice(&lines, Point::new(620.0, 145.0)).unwrap();

    assert_eq!((lower.1, upper.1), (20, 10));
    assert_eq!((lower.1.min(upper.1), lower.2.max(upper.2)), (10, 29));
}

#[test]
fn url_start_accepts_label_and_punctuation_boundaries() {
    for text in [
        "代码仓库：https://github.com/torvalds/linux.git",
        "(https://example.com/a/b)",
    ] {
        let chars = text.chars().collect::<Vec<_>>();
        let ix = chars.iter().position(|c| *c == 'h').unwrap();
        assert!(DocumentView::url_start_boundary(&chars, ix));
    }
}

#[test]
fn url_start_rejects_embedded_prefixes() {
    let chars = "abchttps://example.com/x".chars().collect::<Vec<_>>();
    let ix = chars.iter().position(|c| *c == 'h').unwrap();
    assert!(!DocumentView::url_start_boundary(&chars, ix));
}

#[test]
fn word_bounds_selects_words_between_punctuation() {
    let chars = "hello, world".chars().collect::<Vec<_>>();

    assert_eq!(DocumentView::word_bounds_in_chars(&chars, 1), Some(0..5));
    assert_eq!(DocumentView::word_bounds_in_chars(&chars, 8), Some(7..12));
    assert_eq!(DocumentView::word_bounds_in_chars(&chars, 5), None);
    assert_eq!(DocumentView::word_bounds_in_chars(&chars, 6), None);
}

#[test]
fn word_bounds_includes_apostrophes_underscores_and_unicode() {
    let chars = "can't rust_lang café".chars().collect::<Vec<_>>();
    let apostrophe = chars.iter().position(|c| *c == '\'').unwrap();
    let underscore = chars.iter().position(|c| *c == '_').unwrap();
    let accented = chars.iter().position(|c| *c == 'é').unwrap();

    assert_eq!(
        DocumentView::word_bounds_in_chars(&chars, apostrophe),
        Some(0..5)
    );
    assert_eq!(
        DocumentView::word_bounds_in_chars(&chars, underscore),
        Some(6..15)
    );
    assert_eq!(
        DocumentView::word_bounds_in_chars(&chars, accented),
        Some(16..20)
    );
}

#[test]
fn word_bounds_stops_at_hyphens_and_empty_input() {
    let chars = "foo-bar".chars().collect::<Vec<_>>();

    assert_eq!(DocumentView::word_bounds_in_chars(&chars, 1), Some(0..3));
    assert_eq!(DocumentView::word_bounds_in_chars(&chars, 5), Some(4..7));
    assert_eq!(DocumentView::word_bounds_in_chars(&chars, 3), None);
    assert_eq!(DocumentView::word_bounds_in_chars(&[], 0), None);
}

#[test]
fn word_bounds_stop_at_inferred_visual_spaces() {
    let glyphs = missing_space_glyphs();

    assert_eq!(DocumentView::word_bounds_in_glyphs(&glyphs, 0), Some(0..3));
    assert_eq!(DocumentView::word_bounds_in_glyphs(&glyphs, 3), Some(3..5));
    assert_eq!(DocumentView::word_bounds_in_glyphs(&glyphs, 8), Some(5..12));
}

#[test]
fn selected_text_inserts_inferred_visual_spaces() {
    let glyphs = missing_space_glyphs();

    assert_eq!(
        DocumentView::text_for_glyph_range(&glyphs, 0..glyphs.len()).as_deref(),
        Some("100 by default")
    );
}

#[test]
fn semantic_anchor_survives_shifted_glyph_offsets() {
    let original = "before selected passage after"
        .chars()
        .enumerate()
        .map(|(ix, c)| test_glyph(c, ix as f32, ix as f32 + 1.0))
        .collect::<Vec<_>>();
    let selected = "selected passage";
    let start = "before ".chars().count();
    let anchor = DocumentView::semantic_anchor_for_glyph(&original, start);

    let mut shifted = "preface "
        .chars()
        .enumerate()
        .map(|(ix, c)| test_glyph(c, ix as f32, ix as f32 + 1.0))
        .collect::<Vec<_>>();
    shifted.extend(original);
    let resolved = DocumentView::resolve_semantic_anchor(&shifted, &anchor).unwrap();
    let resolved_text = shifted[resolved..resolved + selected.chars().count()]
        .iter()
        .map(|glyph| glyph.c)
        .collect::<String>();

    assert_eq!(resolved_text, selected);
}

#[test]
fn build_text_blocks_handles_large_glyph_sets_without_rescanning() {
    let mut glyphs = (0..100_000)
        .map(|index| {
            let mut glyph = test_glyph('a', index as f32 * 5.0, index as f32 * 5.0 + 5.0);
            glyph.block_idx = (index / 1000) as u32;
            glyph.line_idx = (index / 100) as u32;
            glyph
        })
        .collect::<Vec<_>>();
    for line in glyphs.chunks_mut(100) {
        DocumentView::mark_inferred_spaces(line);
    }

    let blocks = DocumentView::build_text_blocks(&glyphs);

    assert_eq!(blocks.len(), 100);
    assert!(blocks.iter().all(|block| block.glyph_range.len() == 1000));
}

#[test]
fn merge_text_chunks_offsets_block_and_page_indexes() {
    let first_glyphs = vec![test_glyph('a', 0.0, 5.0)];
    let second_glyphs = vec![test_glyph('b', 0.0, 5.0)];
    let first_bounds = DocumentView::glyph_line_rect(&first_glyphs[0]);
    let second_bounds = DocumentView::glyph_line_rect(&second_glyphs[0]);
    let first = TextChunk {
        start_page_idx: 0,
        text_blocks: DocumentView::build_text_blocks(&first_glyphs),
        glyphs: first_glyphs,
        text_lines: vec![TextLine {
            glyph_range: 0..1,
            bounds: first_bounds,
        }],
        links: Vec::new(),
        page_glyph_ranges: std::iter::once(0..1).collect(),
        page_line_ranges: std::iter::once(0..1).collect(),
        page_block_ranges: std::iter::once(0..1).collect(),
        line_count: 1,
        block_count: 1,
    };
    let second = TextChunk {
        start_page_idx: 1,
        text_blocks: DocumentView::build_text_blocks(&second_glyphs),
        glyphs: second_glyphs,
        text_lines: vec![TextLine {
            glyph_range: 0..1,
            bounds: second_bounds,
        }],
        links: Vec::new(),
        page_glyph_ranges: std::iter::once(0..1).collect(),
        page_line_ranges: std::iter::once(0..1).collect(),
        page_block_ranges: std::iter::once(0..1).collect(),
        line_count: 1,
        block_count: 1,
    };

    let merged = DocumentView::merge_text_chunks(vec![second, first], 2).unwrap();

    assert_eq!(merged.page_glyph_ranges, vec![0..1, 1..2]);
    assert_eq!(merged.page_line_ranges, vec![0..1, 1..2]);
    assert_eq!(merged.page_block_ranges, vec![0..1, 1..2]);
    assert_eq!(merged.text_blocks[0].id, 0);
    assert_eq!(merged.text_blocks[1].id, 1);
    assert_eq!(merged.text_blocks[1].glyph_range, 1..2);
}
