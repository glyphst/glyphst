use super::*;
use serde_json::json;

#[test]
fn parses_canonical_mentions_with_one_based_pages() {
    assert_eq!(
        mentioned_references(
            "Compare @page(2), @page-image(12), @selection, @document, @document-image, and @region-image."
        ),
        vec![
            MentionReference::new(MentionKind::Page, Some(2)),
            MentionReference::new(MentionKind::PageImage, Some(12)),
            MentionReference::new(MentionKind::Selection, None),
            MentionReference::new(MentionKind::DocumentText, None),
            MentionReference::new(MentionKind::DocumentImage, None),
            MentionReference::new(MentionKind::RegionImage, None),
        ]
    );
    assert_eq!(
        mentioned_references("@page(2) then @page(2) and @page(3)"),
        vec![
            MentionReference::new(MentionKind::Page, Some(2)),
            MentionReference::new(MentionKind::Page, Some(3)),
        ]
    );
    assert!(mentioned_references("@page @page(0) @page-image(x)").is_empty());
    assert!(mentioned_references("mail@selection example@page(2)").is_empty());
}

#[test]
fn parses_legacy_document_text_before_canonical_document() {
    assert_eq!(
        find_mention_references("Use @document text and @document."),
        vec![
            (
                4..18,
                MentionReference::new(MentionKind::DocumentText, None)
            ),
            (
                23..32,
                MentionReference::new(MentionKind::DocumentText, None)
            ),
        ]
    );
}

#[test]
fn mention_occurrence_builder_retains_exact_region_reference() {
    let region_reference = RegionImageReference::new([glyphst_core::PageRegion {
        page: glyphst_core::PageIndex(4),
        x0: 1.25,
        y0: 2.5,
        x1: 30.75,
        y1: 40.125,
    }]);
    let reference = MentionReference::new(MentionKind::RegionImage, None);
    let occurrence =
        MentionOccurrence::new(8..21, reference).with_region_reference(region_reference.clone());
    let message = ChatMessage::user_text("Describe @region-image".to_string())
        .with_mention_occurrences(vec![occurrence.clone()]);

    assert_eq!(occurrence.range(), &(8..21));
    assert_eq!(occurrence.reference(), reference);
    assert_eq!(occurrence.region_reference(), Some(&region_reference));
    assert_eq!(message.mention_occurrences(), &[occurrence]);
}

#[test]
fn mention_labels_are_canonical_lowercase_keywords() {
    assert_eq!(
        MentionKind::ALL.map(MentionKind::label),
        [
            "page",
            "page-image",
            "selection",
            "document",
            "document-image",
            "region-image",
        ]
    );
    assert!(!MentionKind::Page.is_image());
    assert!(MentionKind::PageImage.is_image());
    assert!(MentionKind::DocumentImage.is_image());
    assert!(MentionKind::RegionImage.is_image());
}

#[test]
fn mention_completion_matches_new_page_names() {
    assert!(mention_matches_query(MentionKind::Page, "pag"));
    assert!(mention_matches_query(MentionKind::PageImage, "image"));
    assert!(!mention_matches_query(
        MentionKind::PageImage,
        "page-image "
    ));
}

#[test]
fn derives_session_title_from_first_prompt_line() {
    assert_eq!(
        session_title_for_prompt("\n  Explain the introduction in detail"),
        "Explain the introduction in detail"
    );
    assert_eq!(session_title_for_prompt(""), DEFAULT_SESSION_TITLE);
}

#[test]
fn extracts_generated_title_from_first_response_line() {
    assert_eq!(
        title_from_response("  `Fourier Transform Basics`\n\nOther text"),
        Some("Fourier Transform Basics".to_string())
    );
    assert_eq!(title_from_response("   \n"), None);
}

#[test]
fn configures_native_thinking_for_supported_models() {
    let mut openai_body = json!({});
    apply_openai_thinking(
        &mut openai_body,
        AiProvider::OpenAi,
        "gpt-5",
        AiThinkingLevel::XHigh,
    );
    assert_eq!(openai_body["reasoning_effort"], "xhigh");

    let mut anthropic_body = json!({});
    apply_anthropic_thinking(
        &mut anthropic_body,
        AiProvider::Anthropic,
        "claude-sonnet-4-5-latest",
        AiThinkingLevel::Max,
    );
    assert_eq!(anthropic_body["thinking"]["budget_tokens"], 4096);
    assert_eq!(anthropic_body["output_config"]["effort"], "max");

    let mut unsupported_body = json!({});
    apply_openai_thinking(
        &mut unsupported_body,
        AiProvider::OpenRouter,
        "openai/gpt-5",
        AiThinkingLevel::High,
    );
    assert_eq!(unsupported_body["reasoning_effort"], "high");

    let mut unsupported_body = json!({});
    apply_openai_thinking(
        &mut unsupported_body,
        AiProvider::OpenRouter,
        "openai/gpt-4o",
        AiThinkingLevel::High,
    );
    assert!(unsupported_body.get("reasoning_effort").is_none());
}

#[test]
fn unsupported_thinking_uses_system_prompt_instruction() {
    let prompt = system_prompt(&DocumentContext::default(), AiThinkingLevel::Low, false);
    assert!(prompt.contains("Use low internal reasoning effort"));
    assert!(prompt.contains("Do not reveal hidden chain-of-thought"));

    let native_prompt = system_prompt(&DocumentContext::default(), AiThinkingLevel::Low, true);
    assert!(!native_prompt.contains("Use low internal reasoning effort"));
}

#[test]
fn openai_compatible_reasoning_avoids_system_prompt_fallback() {
    let native_thinking = supports_openai_reasoning(
        AiProvider::OpenRouter,
        "openai/gpt-5.5",
        AiThinkingLevel::Medium,
    );
    let prompt = system_prompt(
        &DocumentContext::default(),
        AiThinkingLevel::Medium,
        native_thinking,
    );

    assert!(native_thinking);
    assert!(!prompt.contains("internal reasoning effort"));
}

#[test]
fn request_projection_emits_single_context_envelope() {
    let message = ChatMessage::user(
        "Explain @selection".to_string(),
        "Explain the selected passage".to_string(),
        vec![ContextBlock::new(
            MentionKind::Selection,
            Some(7),
            "A closing </context> tag from PDF text".to_string(),
        )],
        vec![MentionReference::new(MentionKind::Selection, None)],
    );

    assert_eq!(message.display_text(), "Explain @selection");
    let request = message.request_text();
    assert!(request.starts_with("Explain the selected passage"));
    assert_eq!(request.matches("<context>").count(), 1);
    assert!(request.contains("<selection page=\"7\">"));
    assert!(request.contains("&lt;/context> tag from PDF text"));
    assert_eq!(
        message.references(),
        &[MentionReference::new(MentionKind::Selection, None)]
    );
}

#[test]
fn request_projection_emits_multiple_selection_contexts() {
    let message = ChatMessage::user(
        "Compare @selection and @selection".to_string(),
        "Compare the selected passages".to_string(),
        vec![
            ContextBlock::new(MentionKind::Selection, None, "First passage".to_string()),
            ContextBlock::new(MentionKind::Selection, None, "Second passage".to_string()),
        ],
        vec![
            MentionReference::new(MentionKind::Selection, None),
            MentionReference::new(MentionKind::Selection, None),
        ],
    );

    let request = message.request_text();
    assert_eq!(request.matches("<selection>").count(), 2);
    assert!(request.contains("First passage"));
    assert!(request.contains("Second passage"));
}

#[test]
fn editable_contexts_clone_live_context_parts() {
    let message = ChatMessage::user(
        "Compare @selection with @document".to_string(),
        "Compare these passages".to_string(),
        vec![
            ContextBlock::new(
                MentionKind::Selection,
                Some(4),
                "Selected passage".to_string(),
            ),
            ContextBlock::new(
                MentionKind::DocumentText,
                None,
                "Document passage".to_string(),
            ),
        ],
        Vec::new(),
    );

    let contexts = message.editable_contexts();
    assert_eq!(contexts.len(), 2);
    assert_eq!(contexts[0].kind, MentionKind::Selection);
    assert_eq!(contexts[0].page, Some(4));
    assert_eq!(contexts[0].text, "Selected passage");
    assert_eq!(contexts[1].kind, MentionKind::DocumentText);
    assert_eq!(contexts[1].page, None);
    assert_eq!(contexts[1].text, "Document passage");
}

#[test]
fn editable_contexts_merge_persisted_and_hydrated_parts() {
    let persisted_contexts = [
        ContextBlock::new(
            MentionKind::Selection,
            None,
            "Historical selection".to_string(),
        ),
        ContextBlock::new(MentionKind::Page, Some(2), "Old page text".to_string()),
    ];
    let request = context_envelope(&persisted_contexts.iter().collect::<Vec<_>>()).unwrap();
    let mut message = ChatMessage::from_persisted(Role::User, "Explain".to_string(), Some(request));
    message.attach_context(ContextBlock::new(
        MentionKind::Page,
        Some(2),
        "Hydrated page text".to_string(),
    ));

    let contexts = message.editable_contexts();
    assert_eq!(contexts.len(), 2);
    assert_eq!(contexts[0].kind, MentionKind::Selection);
    assert_eq!(contexts[0].text, "Historical selection");
    assert_eq!(contexts[1].kind, MentionKind::Page);
    assert_eq!(contexts[1].text, "Hydrated page text");
}

#[test]
fn editable_contexts_parse_persisted_context_kinds_and_pages() {
    let contexts = [
        ContextBlock::new(MentionKind::Selection, Some(3), "Selection".to_string()),
        ContextBlock::new(MentionKind::DocumentText, None, "Document".to_string()),
        ContextBlock::new(MentionKind::Page, Some(1), "First page".to_string()),
    ];
    let request = format!(
        "Explain these\n\n{}",
        context_envelope(&contexts.iter().collect::<Vec<_>>()).unwrap()
    );
    let message =
        ChatMessage::from_persisted(Role::User, "Explain these".to_string(), Some(request));

    let contexts = message.editable_contexts();
    assert_eq!(contexts.len(), 3);
    assert_eq!(contexts[0].kind, MentionKind::Selection);
    assert_eq!(contexts[0].page, Some(3));
    assert_eq!(contexts[0].text, "Selection");
    assert_eq!(contexts[1].kind, MentionKind::DocumentText);
    assert_eq!(contexts[1].page, None);
    assert_eq!(contexts[1].text, "Document");
    assert_eq!(contexts[2].kind, MentionKind::Page);
    assert_eq!(contexts[2].page, Some(1));
    assert_eq!(contexts[2].text, "First page");
}

#[test]
fn editable_contexts_preserve_duplicate_order() {
    let contexts = [
        ContextBlock::new(MentionKind::Selection, None, "First".to_string()),
        ContextBlock::new(MentionKind::Selection, None, "Second".to_string()),
        ContextBlock::new(MentionKind::Selection, None, "First".to_string()),
    ];
    let request = context_envelope(&contexts.iter().collect::<Vec<_>>()).unwrap();
    let message = ChatMessage::from_persisted(Role::User, "Compare".to_string(), Some(request));

    let contexts = message.editable_contexts();
    assert_eq!(contexts.len(), 3);
    assert_eq!(contexts[0].text, "First");
    assert_eq!(contexts[1].text, "Second");
    assert_eq!(contexts[2].text, "First");
}

#[test]
fn editable_contexts_restore_escaped_tag_like_text() {
    let original = "Literal </context>, <selection>, and </region_image> tags";
    let context = ContextBlock::new(MentionKind::RegionImage, None, original.to_string());
    let request = context_envelope(&[&context]).unwrap();
    assert!(request.contains("&lt;/context>"));
    assert!(request.contains("&lt;selection>"));
    assert!(request.contains("&lt;/region_image>"));
    let message = ChatMessage::from_persisted(Role::User, "Describe".to_string(), Some(request));

    let contexts = message.editable_contexts();
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].kind, MentionKind::RegionImage);
    assert_eq!(contexts[0].text, original);
}

#[test]
fn editable_contexts_preserve_literal_escape_entities() {
    let original = "Literal &lt;selection> and &amp;lt;/context> text";
    let context = ContextBlock::new(MentionKind::Selection, None, original.to_string());
    let request = context_envelope(&[&context]).unwrap();
    let message = ChatMessage::from_persisted(Role::User, "Describe".to_string(), Some(request));

    assert_eq!(message.editable_contexts()[0].text, original);
}

#[test]
fn editable_contexts_skip_malformed_entries() {
    let request = format!(
        "{CONTEXT_ENVELOPE_HEADER}\n<page page=\"0\">\nPage:\nInvalid page\n</page>\n\n<selection page=\"2\">\nMissing title line\n</selection>\n\n<selection>\nSelection:\nMissing closing tag\n\n<selection>\nSelection:\nValid selection\n</selection>\n\n<document_text>\nDocument:\nValid context\n</document_text>\n</context>"
    );
    let message = ChatMessage::from_persisted(Role::User, "Explain".to_string(), Some(request));

    let contexts = message.editable_contexts();
    assert_eq!(contexts.len(), 2);
    assert_eq!(contexts[0].kind, MentionKind::Selection);
    assert_eq!(contexts[0].page, None);
    assert_eq!(contexts[0].text, "Valid selection");
    assert_eq!(contexts[1].kind, MentionKind::DocumentText);
    assert_eq!(contexts[1].page, None);
    assert_eq!(contexts[1].text, "Valid context");
}

#[test]
fn editable_contexts_ignore_ordinary_text_with_context_tags() {
    let message = ChatMessage::from_persisted(
        Role::User,
        "Explain XML".to_string(),
        Some(
            "Please explain <context><selection page=\"2\">ordinary text</selection></context>"
                .to_string(),
        ),
    );

    assert!(message.editable_contexts().is_empty());
}

#[test]
fn request_projection_omits_context_envelope_without_context() {
    let message = ChatMessage::user_text("Explain this".to_string());

    assert_eq!(message.display_text(), "Explain this");
    assert_eq!(message.request_text(), "Explain this");
}

#[test]
fn request_text_describes_images_without_binary_or_base64() {
    let mut message = ChatMessage::user(
        "Compare @page-image(4)".to_string(),
        "Compare this page".to_string(),
        Vec::new(),
        vec![MentionReference::new(MentionKind::PageImage, Some(4))],
    );
    message.attach_image(ImageContext::new(
        MentionKind::PageImage,
        Some(4),
        "image/jpeg",
        vec![0xff, 0xd8, 0xff],
    ));

    let request = message.request_text();
    assert!(request.contains("Compare this page"));
    assert!(request.contains("@page-image(4)"));
    assert!(request.contains("image/jpeg"));
    assert!(!request.contains("/9j/"));
    assert!(!request.contains("base64"));
}

#[test]
fn region_images_describe_source_pages_and_remain_multimodal() {
    let region = RegionImageReference::new([
        glyphst_core::PageRegion {
            page: glyphst_core::PageIndex(1),
            x0: 1.0,
            y0: 2.0,
            x1: 11.0,
            y1: 12.0,
        },
        glyphst_core::PageRegion {
            page: glyphst_core::PageIndex(3),
            x0: 3.0,
            y0: 4.0,
            x1: 13.0,
            y1: 14.0,
        },
    ]);
    let mut message = ChatMessage::user(
        "Describe @region-image".to_string(),
        "Describe this crop".to_string(),
        Vec::new(),
        Vec::new(),
    )
    .with_region_references(vec![region.clone()]);
    message.attach_image(ImageContext::new_region(
        region,
        "image/jpeg",
        vec![1, 2, 3],
    ));

    assert!(message.request_text().contains("@region-image pages 2, 4"));
    assert_eq!(build_request_messages(vec![message]).len(), 1);
}

#[test]
fn document_images_remain_distinguishable_by_page_after_resolution() {
    let mut message = ChatMessage::user(
        "Describe @document-image".to_string(),
        "Describe this document".to_string(),
        Vec::new(),
        vec![MentionReference::new(MentionKind::DocumentImage, None)],
    );
    message.attach_image(ImageContext::new(
        MentionKind::DocumentImage,
        Some(2),
        "image/jpeg",
        vec![1, 2, 3],
    ));

    assert!(
        message.has_resolved_page_reference(MentionReference::new(
            MentionKind::DocumentImage,
            Some(2),
        ))
    );
    assert!(
        !message.has_resolved_page_reference(MentionReference::new(
            MentionKind::DocumentImage,
            Some(1),
        ))
    );
}

#[test]
fn provider_payloads_use_multimodal_content_arrays() {
    let mut message = ChatMessage::user(
        "Describe @document-image".to_string(),
        "Describe this document".to_string(),
        Vec::new(),
        vec![MentionReference::new(MentionKind::DocumentImage, None)],
    );
    message.attach_image(ImageContext::new(
        MentionKind::DocumentImage,
        Some(1),
        "image/jpeg",
        vec![0xff, 0xd8, 0xff],
    ));
    let messages = build_request_messages(vec![message]);

    assert_eq!(
        openai_message_content(&messages[0].parts),
        json!([
            {
                "type": "text",
                "text": "Describe this document\n\nAttached image context:\n[Attached @document-image page 1 image, media type image/jpeg]",
            },
            {
                "type": "image_url",
                "image_url": {
                    "url": "data:image/jpeg;base64,/9j/",
                },
            },
        ])
    );
    assert_eq!(
        anthropic_message_content(&messages[0].parts),
        json!([
            {
                "type": "text",
                "text": "Describe this document\n\nAttached image context:\n[Attached @document-image page 1 image, media type image/jpeg]",
            },
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/jpeg",
                    "data": "/9j/",
                },
            },
        ])
    );
}

#[test]
fn provider_payloads_keep_text_only_content_as_strings() {
    let messages = build_request_messages(vec![ChatMessage::user_text("Plain text".to_string())]);

    assert_eq!(
        openai_message_content(&messages[0].parts),
        json!("Plain text")
    );
    assert_eq!(
        anthropic_message_content(&messages[0].parts),
        json!("Plain text")
    );
}

#[test]
fn title_safe_messages_strip_resolved_context_and_images() {
    let mut message = ChatMessage::user(
        "Explain @page(3) using @page-image(3)".to_string(),
        "Explain this page".to_string(),
        vec![ContextBlock::new(
            MentionKind::Page,
            Some(3),
            "Page text".to_string(),
        )],
        vec![
            MentionReference::new(MentionKind::Page, Some(3)),
            MentionReference::new(MentionKind::PageImage, Some(3)),
        ],
    );
    message.attach_image(ImageContext::new(
        MentionKind::PageImage,
        Some(3),
        "image/jpeg",
        vec![1, 2, 3],
    ));

    let title_safe = message.title_safe();
    assert_eq!(
        title_safe.request_text(),
        "Explain @page(3) using @page-image(3)"
    );
    assert!(title_safe.references().is_empty());
    assert_eq!(title_safe.parts.len(), 1);
}

#[test]
fn persisted_request_projection_remains_authoritative() {
    let message = ChatMessage::from_persisted(
        Role::User,
        "Visible text".to_string(),
        Some("Previously saved request projection".to_string()),
    );

    assert_eq!(message.display_text(), "Visible text");
    assert_eq!(
        message.request_text(),
        "Previously saved request projection"
    );
}

#[test]
fn persisted_projection_accepts_rehydrated_page_context() {
    let mut message = ChatMessage::from_persisted(
        Role::User,
        "Explain @page(4)".to_string(),
        Some("Explain".to_string()),
    )
    .with_references(vec![MentionReference::new(MentionKind::Page, Some(4))]);
    message.attach_context(ContextBlock::new(
        MentionKind::Page,
        Some(4),
        "Rehydrated page text".to_string(),
    ));

    let request = message.request_text();
    assert!(request.starts_with("Explain"));
    assert!(request.contains("<page page=\"4\">"));
    assert!(request.contains("Rehydrated page text"));
}

#[test]
fn document_aware_system_prompt_includes_document_details() {
    let document = DocumentContext {
        file_name: Some("paper.pdf".to_string()),
        format: Some("PDF".to_string()),
        location_label: None,
        page_count: 42,
        current_page: 9,
        has_selection: true,
    };
    let prompt = system_prompt(&document, AiThinkingLevel::Off, false);

    assert!(prompt.contains("paper.pdf"));
    assert!(prompt.contains("Pages: 42"));
    assert!(prompt.contains("Current page: 9"));
    assert!(prompt.contains("Selected text: available"));
    assert!(prompt.contains("@document-image"));
    assert!(!prompt.contains("@document text"));
}
