use super::planning::*;
use super::protection::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use crate::{
    Block, BlockContent, BlockKind, BlockStructure, BoundingBox, ContinuationHint,
    ContinuationScope, DocumentMetadata, Line, Page, PageGeometry, Segment, SegmentKind,
    SourceProvenance, StructureKind, TableOfContentsEntry, TableOfContentsLeader,
    TranslationPolicy,
};

struct EchoBackend {
    calls: Mutex<usize>,
    corrupt_first_placeholder: bool,
}

impl TranslationBackend for EchoBackend {
    fn translate_batch(
        &self,
        units: &[TranslationUnit],
    ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
        let mut calls = self.calls.lock().unwrap();
        *calls += 1;
        Ok(units
            .iter()
            .map(|unit| {
                let text = if self.corrupt_first_placeholder && *calls == 1 {
                    unit.protected.text.replace("[[GLYPHST_", "[[BROKEN_")
                } else {
                    format!("Translated {}", unit.protected.text)
                };
                TranslationUnitOutput {
                    id: unit.id.clone(),
                    text,
                    cached: false,
                    validation_error: None,
                }
            })
            .collect())
    }
}

#[test]
fn placeholders_round_trip() {
    let source = "See [12] and $E = mc^2$ at https://example.com/test.";
    let protected = protect_text(source);

    assert_eq!(
        protected.validate_and_restore(&protected.text).unwrap(),
        source
    );
    assert_eq!(protected.tokens.len(), 3);
}

#[test]
fn adjacent_placeholders_accept_only_inserted_whitespace() {
    let first = ProtectedToken {
        placeholder: "[[GLYPHST_NUMBER_0000_AAAAAAAA]]".to_string(),
        original: "1".to_string(),
        kind: ProtectedTokenKind::NumericLiteral,
        styled_restoration: None,
    };
    let second = ProtectedToken {
        placeholder: "[[GLYPHST_NUMBER_0001_BBBBBBBB]]".to_string(),
        original: "2".to_string(),
        kind: ProtectedTokenKind::NumericLiteral,
        styled_restoration: None,
    };
    let protected = ProtectedText {
        text: format!("{}{}", first.placeholder, second.placeholder),
        tokens: vec![first.clone(), second.clone()],
    };
    let whitespace_separated = format!("{}\n  {}", first.placeholder, second.placeholder);
    let text_separated = format!("{} and {}", first.placeholder, second.placeholder);
    let reordered = format!("{}{}", second.placeholder, first.placeholder);

    assert_eq!(
        protected
            .restore_translation(&whitespace_separated)
            .unwrap()
            .0,
        "12"
    );
    assert!(protected.restore_translation(&text_separated).is_err());
    assert!(protected.restore_translation(&reordered).is_err());
}

#[test]
fn style_boundaries_do_not_force_adjacent_structural_markers() {
    let style_start = ProtectedToken {
        placeholder: "[[GLYPHST_STYLE_START_0000_AAAAAAAA]]".to_string(),
        original: style_marker(0, true),
        kind: ProtectedTokenKind::StyleStart,
        styled_restoration: None,
    };
    let marker = ProtectedToken {
        placeholder: "[[GLYPHST_STRUCTURAL_0001_BBBBBBBB]]".to_string(),
        original: "•".to_string(),
        kind: ProtectedTokenKind::StructuralMarker,
        styled_restoration: None,
    };
    let style_end = ProtectedToken {
        placeholder: "[[GLYPHST_STYLE_END_0002_CCCCCCCC]]".to_string(),
        original: style_marker(0, false),
        kind: ProtectedTokenKind::StyleEnd,
        styled_restoration: None,
    };
    let protected = ProtectedText {
        text: format!(
            "{}{}{}",
            style_start.placeholder, marker.placeholder, style_end.placeholder
        ),
        tokens: vec![style_start.clone(), marker.clone(), style_end.clone()],
    };
    let translated = format!(
        "{}谁会{}参加选举？{}",
        style_start.placeholder, marker.placeholder, style_end.placeholder
    );
    let unit = TranslationUnit {
        id: "unit".to_string(),
        member_block_ids: vec!["block".to_string()],
        role: BlockRole::Prompt,
        source_text: "•Who is running for the election?".to_string(),
        protected,
        style_definitions: vec![StyleDefinition {
            id: 0,
            style: TextStyle {
                bold: true,
                ..TextStyle::default()
            },
        }],
        context_before: None,
        context_after: None,
    };

    let validated = validate_translation(&unit, &translated, false).unwrap();

    assert_eq!(validated.text, "•谁会参加选举？");
    assert!(!validated.style_fallback);
    assert_eq!(validated.runs[0].text, "•");
    assert!(
        validated.runs[0]
            .style
            .as_ref()
            .is_some_and(|style| style.bold)
    );
    assert!(validate_resumed_translation(
        &unit,
        "•谁会参加选举？",
        false
    ));
    assert!(!validate_resumed_translation(
        &unit,
        "谁会•参加选举？",
        false
    ));
}

#[test]
fn duplicate_immutable_placeholders_are_repaired_before_validation() {
    let protected = protect_text("Use 25 cases and cite [12].");
    let number = protected
        .tokens
        .iter()
        .find(|token| token.kind == ProtectedTokenKind::NumericLiteral)
        .unwrap();
    let duplicated = format!(
        "{} and accidentally repeated {}",
        protected.text, number.placeholder
    );

    let repaired = repair_duplicate_protected_placeholders(&protected, &duplicated);

    assert_eq!(repaired.matches(&number.placeholder).count(), 1);
    assert!(
        protected
            .validate_and_restore(&repaired)
            .unwrap()
            .starts_with("Use 25 cases and cite [12].")
    );
}

#[test]
fn missing_immutable_placeholders_are_not_synthesized_by_repair() {
    let protected = protect_text("Use 25 cases.");
    let missing = protected.text.replace(&protected.tokens[0].placeholder, "");

    let repaired = repair_duplicate_protected_placeholders(&protected, &missing);

    assert_eq!(repaired, missing);
    assert!(protected.validate_and_restore(&repaired).is_err());
}

#[test]
fn translation_units_keep_exact_protected_token_metadata() {
    let unit = build_translation_units(&test_document(
        "Use AI for 25 cases; see [12], $x < y$, and https://example.test/?a=1&b=2.",
    ))
    .remove(0);

    assert_eq!(
        unit.protected
            .tokens
            .iter()
            .map(|token| token.kind)
            .collect::<Vec<_>>(),
        [
            ProtectedTokenKind::Identifier,
            ProtectedTokenKind::NumericLiteral,
            ProtectedTokenKind::Citation,
            ProtectedTokenKind::Formula,
            ProtectedTokenKind::Url,
        ]
    );
    for token in &unit.protected.tokens {
        assert!(unit.protected.text.contains(&token.placeholder));
        assert!(!token.original.is_empty());
    }
}

#[test]
fn toc_units_translate_only_the_entry_title() {
    let mut document = test_document("2.1\nPipeline Framework. . . . . . . . .\n4");
    let block = &mut document.pages[0].blocks[0];
    block.role = BlockRole::TableOfContents;
    block.toc_entry = Some(TableOfContentsEntry {
        marker: Some("2.1".to_string()),
        title: "Pipeline Framework".to_string(),
        page_label: "4".to_string(),
        leader: Some(TableOfContentsLeader {
            glyph: ".".to_string(),
            gap_em: 0.32,
        }),
    });

    let unit = build_translation_units(&document).remove(0);

    assert_eq!(unit.source_text, "Pipeline Framework");
    assert_eq!(unit.protected.text, "Pipeline Framework");
    assert!(!unit.protected.text.contains("2.1"));
    assert!(!unit.protected.text.contains('4'));
    assert!(!unit.protected.text.contains('.'));
}

#[test]
fn all_caps_identifier_suffixes_remain_translatable() {
    let protected = protect_text("WORDTOURaligns with people while 20news remains a dataset.");
    let originals = protected
        .tokens
        .iter()
        .map(|token| token.original.as_str())
        .collect::<Vec<_>>();

    assert!(originals.contains(&"WORDTOUR"));
    assert!(originals.contains(&"20news"));
    assert!(!originals.contains(&"WORDTOURaligns"));
    assert!(protected.text.contains("aligns"));

    let sentence = protect_text("Use WORDTOUR. Then compare WMD, BoW; and AI:");
    assert!(sentence.tokens.iter().all(|token| {
        token
            .original
            .chars()
            .last()
            .is_none_or(|character| !matches!(character, '.' | ',' | ';' | ':'))
    }));
}

#[test]
fn numeric_tokens_distinguish_identifiers_from_joined_prose() {
    let protected =
        protect_text("20news, 300-dimensional, Figure2shows, and 3This are distinct cases.");
    let originals = protected
        .tokens
        .iter()
        .filter(|token| token.kind == ProtectedTokenKind::NumericLiteral)
        .map(|token| token.original.as_str())
        .collect::<Vec<_>>();

    assert!(originals.contains(&"20news"));
    assert!(originals.contains(&"300"));
    assert!(originals.contains(&"2"));
    assert!(originals.contains(&"3"));
    assert!(!originals.contains(&"300-dimensional"));
    assert!(!originals.contains(&"2shows"));
    assert!(!originals.contains(&"3This"));

    let punctuation = protect_text("See Table 2. Compare 3.14, then Section 4.3.");
    let numeric = punctuation
        .tokens
        .iter()
        .filter(|token| token.kind == ProtectedTokenKind::NumericLiteral)
        .map(|token| token.original.as_str())
        .collect::<Vec<_>>();
    assert_eq!(numeric, ["2", "3.14", "4.3"]);
}

#[test]
fn heading_words_remain_translatable_while_body_identifiers_are_protected() {
    let heading = protect_text_for_role("1 INTRODUCTION", BlockRole::Heading);
    let body = protect_text_for_role("Use INTRODUCTION as an identifier.", BlockRole::Body);

    assert!(heading.text.contains("INTRODUCTION"));
    assert!(
        !heading
            .tokens
            .iter()
            .any(|token| token.kind == ProtectedTokenKind::Identifier)
    );
    assert!(
        body.tokens
            .iter()
            .any(|token| token.kind == ProtectedTokenKind::Identifier)
    );
}

#[test]
fn pdf_visual_wraps_are_removed_before_translation() {
    let document = test_document(
        "Generative agents wake up, cook breakfast,\nand then plan their day. They inter-\nact with neighbors.",
    );

    let units = build_translation_units(&document);

    assert_eq!(units.len(), 1);
    assert_eq!(
        units[0].source_text,
        "Generative agents wake up, cook breakfast, and then plan their day. They interact with neighbors."
    );
    assert!(!units[0].protected.text.contains('\n'));
}

#[test]
fn structured_list_members_reflow_visual_wraps_and_protect_markers() {
    let mut document = test_document_with_blocks(vec![(
        BlockRole::Prompt,
        "1. Eddy is\nwalking today".into(),
    )]);
    document.pages[0].blocks[0].structure = Some(BlockStructure {
        kind: StructureKind::OrderedListItem,
        flow: TextFlow::Reflow,
        group_id: Some("prompt-group".to_string()),
        group_bbox: Some(BoundingBox::new(10.0, 10.0, 90.0, 80.0)),
        marker: Some("1.".to_string()),
        provider: "paddle".to_string(),
        confidence: 0.98,
    });

    let unit = build_translation_units(&document).remove(0);

    assert_eq!(unit.source_text, "1. Eddy is walking today");
    assert!(!unit.protected.text.contains('\n'));
    let marker = unit
        .protected
        .tokens
        .iter()
        .find(|token| token.kind == ProtectedTokenKind::StructuralMarker)
        .unwrap();
    assert_eq!(marker.original, "1.");
    assert_eq!(
        unit.protected
            .validate_and_restore(&unit.protected.text)
            .unwrap(),
        unit.source_text
    );
}

#[test]
fn structured_members_ignore_native_continuations_and_follow_reading_order() {
    let mut document = test_document_with_blocks(vec![
        (BlockRole::Prompt, "1. First item".into()),
        (BlockRole::Prompt, "2. Second item".into()),
    ]);
    for (index, block) in document.pages[0].blocks.iter_mut().enumerate() {
        block.reading_order = 1 - index;
        block.continuation = Some(ContinuationHint {
            group_id: "native-continuation".to_string(),
            scope: ContinuationScope::IntraPage,
            reading_order: Some(index),
            confidence: 0.98,
        });
        block.structure = Some(BlockStructure {
            kind: StructureKind::OrderedListItem,
            flow: TextFlow::Reflow,
            group_id: Some("ocr-list".to_string()),
            group_bbox: Some(BoundingBox::new(10.0, 10.0, 90.0, 80.0)),
            marker: Some(format!("{}.", index + 1)),
            provider: "paddle".to_string(),
            confidence: 0.98,
        });
    }

    let units = build_translation_units(&document);

    assert_eq!(units.len(), 2);
    assert_eq!(units[0].member_block_ids, ["p001-b0001"]);
    assert_eq!(units[1].member_block_ids, ["p001-b0000"]);
}

#[test]
fn explicit_preserve_lines_flow_overrides_a_body_role() {
    let mut document =
        test_document_with_blocks(vec![(BlockRole::Body, "Speaker A\nSpeaker B".into())]);
    document.pages[0].blocks[0].structure = Some(BlockStructure {
        kind: StructureKind::DialogueTurn,
        flow: TextFlow::PreserveLines,
        group_id: Some("dialogue".to_string()),
        group_bbox: None,
        marker: None,
        provider: "paddle".to_string(),
        confidence: 0.98,
    });

    let unit = build_translation_units(&document).remove(0);

    assert_eq!(unit.source_text, "Speaker A\nSpeaker B");
    assert_eq!(
        unit.protected
            .tokens
            .iter()
            .filter(|token| token.kind == ProtectedTokenKind::LineBreak)
            .count(),
        1
    );
}

#[test]
fn dialogue_units_preserve_speaker_turns_but_join_visual_wraps() {
    let document = test_document_with_blocks(vec![(
        BlockRole::Dialogue,
        "Interviewer: How does this\nsystem work?\nKlaus Mueller: It remembers prior events."
            .to_string(),
    )]);

    let unit = build_translation_units(&document).remove(0);

    assert_eq!(
        unit.source_text,
        "Interviewer: How does this system work?\nKlaus Mueller: It remembers prior events."
    );
    assert!(!unit.protected.text.contains('\n'));
    assert_eq!(
        unit.protected
            .tokens
            .iter()
            .filter(|token| token.kind == ProtectedTokenKind::LineBreak)
            .count(),
        1
    );
    let translated = unit
        .protected
        .text
        .replace(
            "Interviewer: How does this system work?",
            "采访者：这个系统如何工作？",
        )
        .replace(
            "Klaus Mueller: It remembers prior events.",
            "克劳斯·穆勒：它会记住先前的事件。",
        );
    let validated = validate_translation(&unit, &translated, false).unwrap();
    assert_eq!(
        validated.text,
        "采访者：这个系统如何工作？\n克劳斯·穆勒：它会记住先前的事件。"
    );
}

#[test]
fn prompt_units_preserve_structural_lines_and_join_wrapped_prose() {
    let document = test_document_with_blocks(vec![(
            BlockRole::Prompt,
            "[Agent’s Summary Description]\nIt is February 13.\nObservation: John saw Eddy taking a short walk\naround his workplace.\nSummary of relevant context from John’s memory:\nEddy likes music.\nShould John react?"
                .to_string(),
        )]);

    let unit = build_translation_units(&document).remove(0);

    assert_eq!(
        unit.source_text,
        "[Agent’s Summary Description]\nIt is February 13.\nObservation: John saw Eddy taking a short walk around his workplace.\nSummary of relevant context from John’s memory: Eddy likes music.\nShould John react?"
    );
    assert_eq!(
        unit.protected
            .tokens
            .iter()
            .filter(|token| token.kind == ProtectedTokenKind::LineBreak)
            .count(),
        4
    );
}

#[test]
fn prompt_geometry_does_not_promote_a_lowercase_wrapped_colon_to_a_field() {
    let styled_line = |text: &str| {
        text.chars()
            .map(unstyled_character)
            .collect::<Vec<StyledSourceCharacter>>()
    };
    let source_line = |text: &str, y: f32, x1: f32| Line {
        bbox: BoundingBox::new(10.0, y, x1, y + 10.0),
        text: text.to_string(),
        segments: Vec::new(),
        baseline_y: Some(y + 8.0),
        line_height: Some(10.0),
    };
    let lines = vec![
        styled_line("[Agent’s Plan]"),
        styled_line("Today is Wednesday. Here is Eddy’s"),
        styled_line("plan today in broad strokes: 1)"),
    ];
    let source_lines = vec![
        source_line("[Agent’s Plan]", 10.0, 80.0),
        source_line("Today is Wednesday. Here is Eddy’s", 20.0, 190.0),
        source_line("plan today in broad strokes: 1)", 30.0, 165.0),
    ];

    let normalized =
        styled_characters_to_source(normalize_styled_prompt_lines(lines, &source_lines));

    assert_eq!(
        normalized.text,
        "[Agent’s Plan]\nToday is Wednesday. Here is Eddy’s plan today in broad strokes: 1)"
    );
}

#[test]
fn cjk_spacing_is_normalized_across_style_boundaries() {
    let mut runs = vec![
        StyledRun {
            text: "我们使用了  ".to_string(),
            style: None,
        },
        StyledRun {
            text: "300".to_string(),
            style: Some(TextStyle {
                font_class: Some(crate::FontFamilyClass::Math),
                ..TextStyle::default()
            }),
        },
        StyledRun {
            text: " 维的GloVe 用于WMD。".to_string(),
            style: None,
        },
    ];

    normalize_cjk_styled_spacing(&mut runs);

    assert_eq!(
        runs.iter().map(|run| run.text.as_str()).collect::<String>(),
        "我们使用了300维的GloVe用于WMD。"
    );
    assert_eq!(runs[1].text, "300");
    assert!(runs[1].style.is_some());
}

#[test]
fn invented_ai_paragraph_breaks_are_flattened_for_single_paragraph_sources() {
    let normalized = normalize_translated_whitespace(
        "生成式智能体醒来。\n\n它们做早餐。\n\n然后规划一天。",
        "Generative agents wake up. They cook breakfast. Then they plan their day.",
        BlockRole::Body,
    );

    assert_eq!(normalized, "生成式智能体醒来。 它们做早餐。 然后规划一天。");
}

#[test]
fn styled_translation_markers_restore_inline_typography() {
    let definitions = vec![StyleDefinition {
        id: 0,
        style: TextStyle {
            bold: true,
            italic: true,
            color_argb: Some(0xff44_5566),
            ..TextStyle::default()
        },
    }];

    let (plain, runs, fallback) = parse_styled_translation(
        "普通\u{e000}S0\u{e001}重点\u{e000}E0\u{e001}文本",
        &definitions,
    );

    assert_eq!(plain, "普通重点文本");
    assert!(!fallback);
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[1].text, "重点");
    assert!(runs[1].style.as_ref().is_some_and(|style| {
        style.bold && style.italic && style.color_argb == Some(0xff44_5566)
    }));
}

#[test]
fn protected_citations_restore_internal_color_boundaries() {
    let mut document = test_document("Compare (Joachims, 1998) carefully in this discussion.");
    let bbox = document.pages[0].blocks[0].bbox;
    let plain_style = TextStyle {
        color_argb: Some(0xff00_0000),
        ..TextStyle::default()
    };
    let purple_style = TextStyle {
        color_argb: Some(0xff00_00ff),
        ..TextStyle::default()
    };
    let segment = |text: &str, style: TextStyle| Segment {
        kind: SegmentKind::Text,
        text: text.to_string(),
        bbox,
        style,
    };
    let segments = vec![
        segment("Compare (", plain_style.clone()),
        segment("Joachims, 1998", purple_style.clone()),
        segment(") carefully in this discussion.", plain_style),
    ];
    document.pages[0].blocks[0].content.lines[0].segments = segments.clone();
    document.pages[0].blocks[0].content.segments = segments;

    let unit = build_translation_units(&document).remove(0);
    let citation = unit
        .protected
        .tokens
        .iter()
        .find(|token| token.kind == ProtectedTokenKind::Citation)
        .unwrap();
    assert!(
        citation
            .styled_restoration
            .as_deref()
            .is_some_and(|restoration| restoration.contains('\u{e000}'))
    );

    let translated = unit
        .protected
        .text
        .replace("Compare ", "比较")
        .replace(" carefully in this discussion.", "。");
    let validated = validate_translation(&unit, &translated, false).unwrap();
    assert!(validated.runs.iter().any(|run| {
        run.text == "Joachims, 1998"
            && run
                .style
                .as_ref()
                .is_some_and(|style| style.color_argb == purple_style.color_argb)
    }));
}

#[test]
fn malformed_style_markers_fall_back_without_losing_text() {
    let definitions = vec![StyleDefinition {
        id: 0,
        style: TextStyle {
            bold: true,
            ..TextStyle::default()
        },
    }];

    let (plain, runs, fallback) =
        parse_styled_translation("普通\u{e000}S0\u{e001}重点", &definitions);

    assert_eq!(plain, "普通重点");
    assert!(fallback);
    assert_eq!(
        runs,
        vec![StyledRun {
            text: plain,
            style: None
        }]
    );
}

#[test]
fn repairs_placeholder_corruption() {
    let document = test_document("Equation $x + y$ is discussed in [3].");
    let backend = EchoBackend {
        calls: Mutex::new(0),
        corrupt_first_placeholder: true,
    };

    let translated =
        translate_document(document, &backend, &CancellationToken::new(), |_, _| {}).unwrap();

    assert!(translated.units[0].translated_text.contains("$x + y$"));
    assert!(translated.units[0].translated_text.contains("[3]"));
    assert!(*backend.calls.lock().unwrap() >= 2);
}

#[test]
fn blocks_export_when_repairs_fail() {
    struct InvalidBackend {
        calls: AtomicUsize,
    }
    impl TranslationBackend for InvalidBackend {
        fn translate_batch(
            &self,
            units: &[TranslationUnit],
        ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(units
                .iter()
                .map(|unit| TranslationUnitOutput {
                    id: unit.id.clone(),
                    text: String::new(),
                    cached: false,
                    validation_error: None,
                })
                .collect())
        }
    }

    let backend = InvalidBackend {
        calls: AtomicUsize::new(0),
    };
    let last_snapshot = Arc::new(Mutex::new(None));
    let captured = Arc::clone(&last_snapshot);
    let error = translate_document_resuming(
        test_document("This paragraph must be translated."),
        &backend,
        &CancellationToken::new(),
        Vec::new(),
        move |snapshot| {
            *captured.lock().unwrap() = Some(snapshot.clone());
            Ok(())
        },
    )
    .unwrap_err();

    assert_eq!(error.unresolved_unit_ids, ["unit-p001-b0000"]);
    assert!(error.retryable);
    assert_eq!(backend.calls.load(Ordering::SeqCst), MAX_UNIT_ATTEMPTS);
    let snapshot = last_snapshot.lock().unwrap().clone().unwrap();
    assert_eq!(snapshot.progress.failed, 1);
    assert_eq!(snapshot.failures[0].attempts, MAX_UNIT_ATTEMPTS);
    assert_eq!(snapshot.queue.retry_requests, MAX_UNIT_ATTEMPTS - 1);
    assert_eq!(
        snapshot.queue.validation_retry_requests,
        MAX_UNIT_ATTEMPTS - 1
    );
    assert_eq!(snapshot.queue.provider_retry_requests, 0);
    assert_eq!(snapshot.queue.completed_requests, MAX_UNIT_ATTEMPTS);
    assert!(snapshot.queue.max_request_latency_ms <= snapshot.queue.total_request_latency_ms);
}

#[test]
fn provider_recovery_diagnostic_stops_identical_scheduler_retries() {
    struct ProviderRecoveryBackend {
        calls: AtomicUsize,
    }

    impl TranslationBackend for ProviderRecoveryBackend {
        fn translate_batch(
            &self,
            units: &[TranslationUnit],
        ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(units
                .iter()
                .map(|unit| TranslationUnitOutput {
                    id: unit.id.clone(),
                    text: unit.protected.text.clone(),
                    cached: false,
                    validation_error: Some(
                        "Google semantic recovery failed after segmented HTML; batch HTML \
                             token mismatch; singleton HTML token mismatch; segmented HTML token \
                             mismatch"
                            .to_string(),
                    ),
                })
                .collect())
        }
    }

    let backend = ProviderRecoveryBackend {
        calls: AtomicUsize::new(0),
    };
    let last_snapshot = Arc::new(Mutex::new(None));
    let captured = Arc::clone(&last_snapshot);
    let error = translate_document_resuming(
        test_document("This paragraph includes [12] and must be translated."),
        &backend,
        &CancellationToken::new(),
        Vec::new(),
        move |snapshot| {
            *captured.lock().unwrap() = Some(snapshot.clone());
            Ok(())
        },
    )
    .unwrap_err();

    assert_eq!(error.unresolved_unit_ids, ["unit-p001-b0000"]);
    assert!(!error.retryable);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    let snapshot = last_snapshot.lock().unwrap().clone().unwrap();
    assert_eq!(snapshot.failures[0].attempts, 1);
    assert!(!snapshot.failures[0].retryable);
    assert!(snapshot.failures[0].reason.contains("batch HTML"));
    assert!(snapshot.failures[0].reason.contains("singleton HTML"));
    assert!(snapshot.failures[0].reason.contains("segmented HTML"));
}

#[test]
fn permanent_backend_failures_are_not_retryable() {
    struct PermanentBackend {
        calls: AtomicUsize,
    }
    impl TranslationBackend for PermanentBackend {
        fn translate_batch(
            &self,
            _units: &[TranslationUnit],
        ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(TranslationBackendError {
                category: Some(TranslationErrorCategory::Authentication),
                message: "credentials are invalid".to_string(),
                status: Some(401),
                retryable: false,
                retry_after: None,
            })
        }
    }

    let backend = PermanentBackend {
        calls: AtomicUsize::new(0),
    };
    let error = translate_document(
        test_document("This paragraph must be translated."),
        &backend,
        &CancellationToken::new(),
        |_, _| {},
    )
    .unwrap_err();

    assert!(!error.retryable);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn worker_panics_become_retryable_failures() {
    struct PanicBackend;
    impl TranslationBackend for PanicBackend {
        fn translate_batch(
            &self,
            _units: &[TranslationUnit],
        ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
            panic!("worker panic")
        }
    }

    let last_snapshot = Arc::new(Mutex::new(None));
    let captured = Arc::clone(&last_snapshot);
    let error = translate_document_resuming(
        test_document("This paragraph must be translated."),
        &PanicBackend,
        &CancellationToken::new(),
        Vec::new(),
        move |snapshot| {
            *captured.lock().unwrap() = Some(snapshot.clone());
            Ok(())
        },
    )
    .unwrap_err();

    assert!(error.retryable);
    let snapshot = last_snapshot.lock().unwrap().clone().unwrap();
    assert_eq!(
        snapshot.queue.provider_retry_requests,
        MAX_UNIT_ATTEMPTS - 1
    );
    assert_eq!(snapshot.queue.validation_retry_requests, 0);
    assert!(snapshot.queue.elapsed_ms >= 500);
}

#[test]
fn risk_aware_planning_batches_only_small_body_units() {
    let mut blocks = vec![
        (BlockRole::Heading, "A heading".to_string()),
        (BlockRole::FigureCaption, "A caption".to_string()),
        (BlockRole::Body, "long ".repeat(500)),
    ];
    blocks.extend((0..7).map(|index| (BlockRole::Body, format!("Body paragraph number {index}."))));
    let document = test_document_with_blocks(blocks);
    let units = build_translation_units(&document);

    let plans = plan_translation_requests(&units);

    assert_eq!(plans[0].units.len(), 1);
    assert!(plans[0].complex);
    assert_eq!(plans[1].units.len(), 1);
    assert!(plans[1].complex);
    assert_eq!(plans[2].units.len(), 1);
    assert!(plans[2].complex);
    assert_eq!(plans[3].units.len(), 6);
    assert!(!plans[3].complex);
    assert_eq!(plans[4].units.len(), 1);
}

#[test]
fn unmapped_glyph_blocks_never_reach_translation_or_layout() {
    struct NoCallBackend;

    impl TranslationBackend for NoCallBackend {
        fn translate_batch(
            &self,
            _units: &[TranslationUnit],
        ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
            panic!("unmapped glyph block must not reach the translation backend")
        }
    }

    let translated = translate_document(
        test_document("\u{fffd}"),
        &NoCallBackend,
        &CancellationToken::new(),
        |_, _| {},
    )
    .unwrap();

    assert!(translated.units.is_empty());
    let block = &translated.document.pages[0].blocks[0];
    assert!(!block.policy.translate);
    assert!(block.policy.locked);
    assert_eq!(block.policy.reason, "unmapped_glyph_source");
    let layout =
        crate::build_layout_with_options(&translated, crate::LayoutOptions::application_default());
    assert!(layout.pages[0].items.is_empty());
}

#[test]
fn bounded_parallel_translation_preserves_document_order() {
    struct ConcurrentBackend {
        active: AtomicUsize,
        peak: AtomicUsize,
        active_complex: AtomicUsize,
        peak_complex: AtomicUsize,
    }

    impl TranslationBackend for ConcurrentBackend {
        fn translate_batch(
            &self,
            units: &[TranslationUnit],
        ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            update_peak(&self.peak, active);
            let complex = units.len() == 1 && units[0].role == BlockRole::Heading;
            if complex {
                let active_complex = self.active_complex.fetch_add(1, Ordering::SeqCst) + 1;
                update_peak(&self.peak_complex, active_complex);
            }
            let delay = units
                .first()
                .and_then(|unit| unit.member_block_ids[0].rsplit('-').next())
                .and_then(|value| value.parse::<usize>().ok())
                .map(|index| 15 + (index % 4) * 10)
                .unwrap_or(25);
            thread::sleep(Duration::from_millis(delay as u64));
            if complex {
                self.active_complex.fetch_sub(1, Ordering::SeqCst);
            }
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(valid_outputs(units))
        }
    }

    let mut blocks = (0..4)
        .map(|index| (BlockRole::Heading, format!("Heading {index}")))
        .collect::<Vec<_>>();
    blocks
        .extend((0..36).map(|index| (BlockRole::Body, format!("Body paragraph number {index}."))));
    let document = test_document_with_blocks(blocks);
    let expected_ids = build_translation_units(&document)
        .into_iter()
        .map(|unit| unit.id)
        .collect::<Vec<_>>();
    let backend = ConcurrentBackend {
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        active_complex: AtomicUsize::new(0),
        peak_complex: AtomicUsize::new(0),
    };
    let last_snapshot = Arc::new(Mutex::new(None));
    let captured = Arc::clone(&last_snapshot);

    let translated = translate_document_resuming(
        document,
        &backend,
        &CancellationToken::new(),
        Vec::new(),
        move |snapshot| {
            *captured.lock().unwrap() = Some(snapshot.clone());
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(
        translated
            .units
            .iter()
            .map(|unit| unit.id.clone())
            .collect::<Vec<_>>(),
        expected_ids
    );
    assert_eq!(backend.peak.load(Ordering::SeqCst), MAX_ACTIVE_REQUESTS);
    assert_eq!(
        backend.peak_complex.load(Ordering::SeqCst),
        MAX_ACTIVE_COMPLEX_REQUESTS
    );
    let snapshot = last_snapshot.lock().unwrap().clone().unwrap();
    assert_eq!(snapshot.queue.peak_active_requests, MAX_ACTIVE_REQUESTS);
    assert_eq!(
        snapshot.queue.peak_active_complex_requests,
        MAX_ACTIVE_COMPLEX_REQUESTS
    );
}

#[test]
fn retains_valid_partial_results_and_retries_only_omitted_units() {
    struct PartialBackend {
        attempts: Mutex<HashMap<String, usize>>,
    }

    impl TranslationBackend for PartialBackend {
        fn translate_batch(
            &self,
            units: &[TranslationUnit],
        ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
            let mut attempts = self.attempts.lock().unwrap();
            for unit in units {
                *attempts.entry(unit.id.clone()).or_default() += 1;
            }
            let omit_last = units.len() > 1;
            Ok(units
                .iter()
                .take(units.len() - usize::from(omit_last))
                .map(valid_output)
                .collect())
        }
    }

    let document = test_document_with_blocks(
        (0..3)
            .map(|index| {
                (
                    BlockRole::Body,
                    format!("Paragraph {index} needs translation."),
                )
            })
            .collect(),
    );
    let ids = build_translation_units(&document)
        .into_iter()
        .map(|unit| unit.id)
        .collect::<Vec<_>>();
    let backend = PartialBackend {
        attempts: Mutex::new(HashMap::new()),
    };

    let translated =
        translate_document(document, &backend, &CancellationToken::new(), |_, _| {}).unwrap();

    assert_eq!(translated.units.len(), 3);
    let attempts = backend.attempts.lock().unwrap();
    assert_eq!(attempts[&ids[0]], 1);
    assert_eq!(attempts[&ids[1]], 1);
    assert_eq!(attempts[&ids[2]], 2);
}

#[test]
fn final_single_recovery_resolves_poisoned_batch_output() {
    struct RecoveryBackend {
        recovery_calls: AtomicUsize,
    }

    impl TranslationBackend for RecoveryBackend {
        fn translate_batch(
            &self,
            units: &[TranslationUnit],
        ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
            Ok(units
                .iter()
                .map(|unit| TranslationUnitOutput {
                    id: unit.id.clone(),
                    text: String::new(),
                    cached: false,
                    validation_error: None,
                })
                .collect())
        }

        fn supports_final_single_recovery(&self) -> bool {
            true
        }

        fn translate_single_recovery(
            &self,
            unit: &TranslationUnit,
        ) -> Result<TranslationUnitOutput, TranslationBackendError> {
            self.recovery_calls.fetch_add(1, Ordering::SeqCst);
            Ok(valid_output(unit))
        }
    }

    let backend = RecoveryBackend {
        recovery_calls: AtomicUsize::new(0),
    };
    let translated = translate_document(
        test_document("This paragraph must be translated."),
        &backend,
        &CancellationToken::new(),
        |_, _| {},
    )
    .unwrap();

    assert_eq!(translated.units.len(), 1);
    assert_eq!(backend.recovery_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn retry_delay_honors_complete_provider_hint() {
    assert_eq!(
        retry_delay(1, Some(Duration::from_secs(2))),
        Duration::from_secs(2)
    );
    assert_eq!(
        retry_delay(1, Some(Duration::from_secs(20))),
        Duration::from_secs(20)
    );
    assert_eq!(retry_delay(2, None), Duration::from_millis(500));
}

#[test]
fn resumed_units_skip_backend_requests() {
    struct PanicBackend;
    impl TranslationBackend for PanicBackend {
        fn translate_batch(
            &self,
            _units: &[TranslationUnit],
        ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
            panic!("checkpointed unit should not be requested")
        }
    }

    let document = test_document("This paragraph was translated previously.");
    let unit = build_translation_units(&document).remove(0);
    let resumed = TranslatedUnit {
        id: unit.id.clone(),
        member_block_ids: unit.member_block_ids.clone(),
        role: unit.role,
        source_text: unit.source_text.clone(),
        translated_text: "Previously translated paragraph.".to_string(),
        styled_runs: Vec::new(),
        style_fallback: false,
        semantic_hash: semantic_unit_hash(&unit),
    };
    let last_snapshot = Arc::new(Mutex::new(None));
    let captured = Arc::clone(&last_snapshot);

    let translated = translate_document_resuming(
        document,
        &PanicBackend,
        &CancellationToken::new(),
        vec![resumed],
        move |snapshot| {
            *captured.lock().unwrap() = Some(snapshot.clone());
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(translated.units.len(), 1);
    let snapshot = last_snapshot.lock().unwrap().clone().unwrap();
    assert_eq!(snapshot.progress.cached, 1);
    assert_eq!(snapshot.queue.dispatched_requests, 0);
}

#[test]
fn resumed_styled_units_do_not_require_internal_style_markers() {
    let unit = TranslationUnit {
        id: "styled".to_string(),
        member_block_ids: vec!["block".to_string()],
        role: BlockRole::Body,
        source_text: "Styled source text".to_string(),
        protected: ProtectedText {
            text: "[[GLYPHST_STYLE_START_0000_AAAAAAAA]]source[[GLYPHST_STYLE_END_0001_BBBBBBBB]]"
                .to_string(),
            tokens: vec![
                ProtectedToken {
                    placeholder: "[[GLYPHST_STYLE_START_0000_AAAAAAAA]]".to_string(),
                    original: style_marker(0, true),
                    kind: ProtectedTokenKind::StyleStart,
                    styled_restoration: None,
                },
                ProtectedToken {
                    placeholder: "[[GLYPHST_STYLE_END_0001_BBBBBBBB]]".to_string(),
                    original: style_marker(0, false),
                    kind: ProtectedTokenKind::StyleEnd,
                    styled_restoration: None,
                },
            ],
        },
        style_definitions: Vec::new(),
        context_before: None,
        context_after: None,
    };

    assert!(validate_resumed_translation(
        &unit,
        "已翻译的样式文本",
        true
    ));
}

#[test]
fn semantic_hash_includes_neighboring_context() {
    let document = test_document("This paragraph was translated previously.");
    let mut unit = build_translation_units(&document).remove(0);
    let original = semantic_unit_hash(&unit);
    unit.context_after = Some("A changed neighboring paragraph.".to_string());

    assert_ne!(semantic_unit_hash(&unit), original);
}

#[test]
fn cancellation_stops_queued_translation_work() {
    struct SlowBackend {
        calls: AtomicUsize,
    }

    impl TranslationBackend for SlowBackend {
        fn translate_batch(
            &self,
            units: &[TranslationUnit],
        ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(80));
            Ok(valid_outputs(units))
        }
    }

    let document = test_document_with_blocks(
        (0..48)
            .map(|index| {
                (
                    BlockRole::Body,
                    format!("Paragraph {index} needs translation."),
                )
            })
            .collect(),
    );
    let backend = SlowBackend {
        calls: AtomicUsize::new(0),
    };
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(15));
        cancel.cancel();
    });

    let error = translate_document(document, &backend, &cancellation, |_, _| {}).unwrap_err();
    canceller.join().unwrap();

    assert_eq!(error.message, "PDF translation was cancelled");
    assert!(backend.calls.load(Ordering::SeqCst) <= MAX_ACTIVE_REQUESTS);
}

fn update_peak(peak: &AtomicUsize, value: usize) {
    let mut observed = peak.load(Ordering::SeqCst);
    while value > observed {
        match peak.compare_exchange(observed, value, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => break,
            Err(next) => observed = next,
        }
    }
}

fn valid_output(unit: &TranslationUnit) -> TranslationUnitOutput {
    TranslationUnitOutput {
        id: unit.id.clone(),
        text: format!("Translated {}", unit.protected.text),
        cached: false,
        validation_error: None,
    }
}

fn valid_outputs(units: &[TranslationUnit]) -> Vec<TranslationUnitOutput> {
    units.iter().map(valid_output).collect()
}

fn test_document(text: &str) -> DocumentV1 {
    test_document_with_blocks(vec![(BlockRole::Body, text.to_string())])
}

fn test_document_with_blocks(blocks: Vec<(BlockRole, String)>) -> DocumentV1 {
    let bbox = BoundingBox::new(10.0, 10.0, 90.0, 40.0);
    DocumentV1::new(
        DocumentMetadata {
            document_id: "test".to_string(),
            source_path: "test.pdf".into(),
            provider: "test".to_string(),
            provider_version: "1".to_string(),
            extra: BTreeMap::new(),
        },
        vec![Page {
            page_index: 0,
            geometry: PageGeometry::new(100.0, 100.0),
            blocks: blocks
                .into_iter()
                .enumerate()
                .map(|(index, (role, text))| Block {
                    id: format!("p001-b{index:04}"),
                    page_index: 0,
                    order: index,
                    reading_order: index,
                    kind: BlockKind::Text,
                    role,
                    bbox,
                    content: BlockContent {
                        text: text.clone(),
                        lines: vec![Line {
                            bbox,
                            text,
                            segments: Vec::new(),
                            baseline_y: None,
                            line_height: Some(bbox.height()),
                        }],
                        segments: Vec::new(),
                        markdown: None,
                        html: None,
                    },
                    policy: TranslationPolicy::translate("test"),
                    continuation: None,
                    structure: None,
                    toc_entry: None,
                    source: SourceProvenance {
                        provider: "test".to_string(),
                        raw_label: "text".to_string(),
                        raw_bbox: bbox,
                        raw_path: None,
                    },
                    tags: Vec::new(),
                    metadata: BTreeMap::new(),
                })
                .collect(),
            protected_regions: Vec::new(),
            metadata: BTreeMap::new(),
        }],
    )
}
