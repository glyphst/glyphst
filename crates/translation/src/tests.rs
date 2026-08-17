use super::cache::*;
use std::fs;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;
use glyphst_settings::{AiApiStyle, AiModel, ModelRef};

#[test]
fn language_code_normalizes_and_validates() {
    assert_eq!(LanguageCode::new(" Zh-Hans ").unwrap().as_str(), "zh-hans");
    assert!(LanguageCode::new("bad value").is_err());
}

#[test]
fn target_language_catalog_is_unique_and_valid() {
    let mut codes = std::collections::HashSet::new();
    for language in TARGET_LANGUAGES {
        assert!(codes.insert(language.code));
        assert_eq!(
            LanguageCode::new(language.code).unwrap().as_str(),
            language.code
        );
        assert!(!language.label.is_empty());
    }
}

#[test]
fn split_batches_respect_limits() {
    let texts = (0..MAX_BATCH_TEXTS + 1)
        .map(|_| "test".to_string())
        .collect();
    let batches = split_batches(texts);
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].len(), MAX_BATCH_TEXTS);
}

#[test]
fn provider_language_mappings_are_stable() {
    assert_eq!(deepl_language("zh-hant"), "ZH-HANT");
    assert_eq!(google_language("zh-hans"), "zh-CN");
    assert_eq!(microsoft_language("zh-tw"), "zh-Hant");
}

#[test]
fn google_semantic_html_uses_exact_protected_spans_without_opaque_tokens() {
    let unit = test_semantic_unit(
        "unit-google-html",
        "Use [[GLYPHST_IDENTIFIER_0000_AAAAAAAA]] at \
             [[GLYPHST_NUMBER_0001_BBBBBBBB]] with \
             [[GLYPHST_FORMULA_0002_CCCCCCCC]], \
             [[GLYPHST_CITATION_0003_DDDDDDDD]], and \
             [[GLYPHST_URL_0004_EEEEEEEE]].",
        vec![
            test_semantic_token(
                "[[GLYPHST_IDENTIFIER_0000_AAAAAAAA]]",
                "AI & ML",
                SemanticProtectedTokenKind::Identifier,
            ),
            test_semantic_token(
                "[[GLYPHST_NUMBER_0001_BBBBBBBB]]",
                "25",
                SemanticProtectedTokenKind::NumericLiteral,
            ),
            test_semantic_token(
                "[[GLYPHST_FORMULA_0002_CCCCCCCC]]",
                "$x < y$",
                SemanticProtectedTokenKind::Formula,
            ),
            test_semantic_token(
                "[[GLYPHST_CITATION_0003_DDDDDDDD]]",
                "[6]",
                SemanticProtectedTokenKind::Citation,
            ),
            test_semantic_token(
                "[[GLYPHST_URL_0004_EEEEEEEE]]",
                "https://example.test/?a=1&b=2",
                SemanticProtectedTokenKind::Url,
            ),
        ],
    );

    let prepared = prepare_google_semantic_unit(&unit).unwrap();

    assert!(!prepared.html.contains("[[GLYPHST_"));
    assert!(prepared.html.contains("<span>AI &amp; ML</span>"));
    assert!(prepared.html.contains("<span>25</span>"));
    assert!(
        prepared
            .html
            .contains("<span translate=\"no\" class=\"notranslate\">$x &lt; y$</span>")
    );
    assert!(prepared.html.contains(
        "<span translate=\"no\" class=\"notranslate\">https://example.test/?a=1&amp;b=2</span>"
    ));
    assert_eq!(
        prepared
            .html
            .matches("translate=\"no\" class=\"notranslate\"")
            .count(),
        3
    );

    let translated_html = prepared.html.replacen("Use", "Utiliser", 1);
    assert_eq!(
        restore_google_semantic_html(&prepared, &translated_html, "batch HTML").unwrap(),
        unit.text.replacen("Use", "Utiliser", 1)
    );
    assert_eq!(html_unescape("&amp;lt;"), "&lt;");
}

#[test]
fn google_semantic_html_reports_the_complete_token_mismatch() {
    let unit = test_semantic_unit(
        "unit-google-mismatch",
        "See [[GLYPHST_CITATION_0000_AAAAAAAA]] at \
             [[GLYPHST_NUMBER_0001_BBBBBBBB]].",
        vec![
            test_semantic_token(
                "[[GLYPHST_CITATION_0000_AAAAAAAA]]",
                "[6]",
                SemanticProtectedTokenKind::Citation,
            ),
            test_semantic_token(
                "[[GLYPHST_NUMBER_0001_BBBBBBBB]]",
                "25",
                SemanticProtectedTokenKind::NumericLiteral,
            ),
        ],
    );
    let prepared = prepare_google_semantic_unit(&unit).unwrap();
    let corrupted = prepared.html.replace("[6]", "[7]").replace("25", "二十五");

    let error = restore_google_semantic_html(&prepared, &corrupted, "singleton HTML").unwrap_err();

    assert!(error.contains("singleton HTML token mismatch"));
    assert!(error.contains("[[GLYPHST_CITATION_0000_AAAAAAAA]]"));
    assert!(error.contains("[[GLYPHST_NUMBER_0001_BBBBBBBB]]"));
    assert!(error.contains("expected 2 protected spans, found 2"));
}

#[test]
fn google_semantic_html_round_trips_protected_line_breaks() {
    let placeholder = "[[GLYPHST_LINE_BREAK_0000_AAAAAAAA]]";
    let unit = test_semantic_unit(
        "unit-google-line-break",
        &format!("Speaker one: Hello.{placeholder}Speaker two: Hi."),
        vec![test_semantic_token(
            placeholder,
            "\n",
            SemanticProtectedTokenKind::LineBreak,
        )],
    );

    let prepared = prepare_google_semantic_unit(&unit).unwrap();
    assert!(
        prepared
            .html
            .contains("<span translate=\"no\" class=\"notranslate\">\n</span>")
    );
    assert_eq!(
        restore_google_semantic_html(&prepared, &prepared.html, "batch HTML").unwrap(),
        unit.text
    );
}

#[test]
fn google_unchanged_detection_ignores_token_only_units() {
    let placeholder = "[[GLYPHST_NUMBER_0000_AAAAAAAA]]";
    let unit = test_semantic_unit(
        "unit-google-token-only",
        placeholder,
        vec![test_semantic_token(
            placeholder,
            "(1)",
            SemanticProtectedTokenKind::NumericLiteral,
        )],
    );
    let prepared = prepare_google_semantic_unit(&unit).unwrap();
    let source = LanguageCode::new("en").unwrap();
    let target = LanguageCode::new("zh-hans").unwrap();

    assert!(
        validate_google_semantic_output(
            &unit,
            &prepared,
            &unit.text,
            Some(&source),
            &target,
            "test",
        )
        .is_ok()
    );
}

#[test]
fn google_unchanged_detection_still_rejects_natural_language() {
    let unit = test_semantic_unit(
        "unit-google-natural-language",
        "This sentence still needs translation.",
        Vec::new(),
    );
    let prepared = prepare_google_semantic_unit(&unit).unwrap();
    let source = LanguageCode::new("en").unwrap();
    let target = LanguageCode::new("zh-hans").unwrap();

    assert!(
        validate_google_semantic_output(
            &unit,
            &prepared,
            &unit.text,
            Some(&source),
            &target,
            "test",
        )
        .is_err()
    );
}

#[test]
fn google_segmented_recovery_reassembles_natural_language_deterministically() {
    let unit = test_semantic_unit(
        "unit-google-segments",
        "At [[GLYPHST_NUMBER_0000_AAAAAAAA]], use \
             [[GLYPHST_IDENTIFIER_0001_BBBBBBBB]] with \
             [[GLYPHST_FORMULA_0002_CCCCCCCC]].",
        vec![
            test_semantic_token(
                "[[GLYPHST_NUMBER_0000_AAAAAAAA]]",
                "11:00",
                SemanticProtectedTokenKind::NumericLiteral,
            ),
            test_semantic_token(
                "[[GLYPHST_IDENTIFIER_0001_BBBBBBBB]]",
                "AI",
                SemanticProtectedTokenKind::Identifier,
            ),
            test_semantic_token(
                "[[GLYPHST_FORMULA_0002_CCCCCCCC]]",
                "$x+y$",
                SemanticProtectedTokenKind::Formula,
            ),
        ],
    );
    let prepared = prepare_google_semantic_unit(&unit).unwrap();

    let translated = reassemble_google_segments(
        &prepared,
        &["在".to_string(), "，使用".to_string(), "结合".to_string()],
    );

    assert_eq!(
        translated,
        "在[[GLYPHST_NUMBER_0000_AAAAAAAA]]，使用\
             [[GLYPHST_IDENTIFIER_0001_BBBBBBBB]]结合\
             [[GLYPHST_FORMULA_0002_CCCCCCCC]]."
    );
    assert!(validate_semantic_cache_entry(&unit, &translated));
}

#[test]
fn failed_google_artifact_units_prepare_and_restore_without_opaque_transport_tokens() {
    let fixtures = [
        (
            "unit-p002-b0010",
            "[[GLYPHST_CITATION_0000_9CF3EBE2]]",
            "[21]",
            SemanticProtectedTokenKind::Citation,
        ),
        (
            "unit-p002-b0015",
            "[[GLYPHST_FORMULA_0000_312AAB08]]",
            "$ ^{1} $",
            SemanticProtectedTokenKind::Formula,
        ),
        (
            "unit-p003-b0009",
            "[[GLYPHST_IDENTIFIER_0000_23D5135F]]",
            "AI",
            SemanticProtectedTokenKind::Identifier,
        ),
        (
            "unit-p003-b0016",
            "[[GLYPHST_IDENTIFIER_0003_1B0E2339]]",
            "NPCs",
            SemanticProtectedTokenKind::Identifier,
        ),
        (
            "unit-p004-b0002",
            "[[GLYPHST_IDENTIFIER_0009_54497523]]",
            "OpenAI",
            SemanticProtectedTokenKind::Identifier,
        ),
        (
            "unit-p004-b0009",
            "[[GLYPHST_CITATION_0000_B61964BB]]",
            "[6]",
            SemanticProtectedTokenKind::Citation,
        ),
        (
            "unit-p006-b0032",
            "[[GLYPHST_NUMBER_0000_7470CFB1]]",
            "3.4.1",
            SemanticProtectedTokenKind::NumericLiteral,
        ),
        (
            "unit-p007-b0015",
            "[[GLYPHST_NUMBER_0000_7270CC8B]]",
            "3.4.3",
            SemanticProtectedTokenKind::NumericLiteral,
        ),
        (
            "unit-p010-b0009",
            "[[GLYPHST_NUMBER_0000_21EB33B6]]",
            "1.",
            SemanticProtectedTokenKind::NumericLiteral,
        ),
        (
            "unit-p010-b0010",
            "[[GLYPHST_NUMBER_0000_A3F2BC21]]",
            "2.",
            SemanticProtectedTokenKind::NumericLiteral,
        ),
        (
            "unit-p010-b0011",
            "[[GLYPHST_NUMBER_0000_81F04804]]",
            "3.",
            SemanticProtectedTokenKind::NumericLiteral,
        ),
        (
            "unit-p010-b0012",
            "[[GLYPHST_NUMBER_0000_300CA0D0]]",
            "5",
            SemanticProtectedTokenKind::NumericLiteral,
        ),
        (
            "unit-p011-b0004",
            "[[GLYPHST_FORMULA_0000_9B2CF841]]",
            "$ ^{4} $",
            SemanticProtectedTokenKind::Formula,
        ),
        (
            "unit-p011-b0011",
            "[[GLYPHST_NUMBER_0010_15B222CF]]",
            "11:00",
            SemanticProtectedTokenKind::NumericLiteral,
        ),
        (
            "unit-p015-b0004",
            "[[GLYPHST_NUMBER_0000_223579D8]]",
            "6.5.2",
            SemanticProtectedTokenKind::NumericLiteral,
        ),
        (
            "unit-p015-b0006",
            "[[GLYPHST_NUMBER_0000_A27C5AA9]]",
            "18th-century",
            SemanticProtectedTokenKind::NumericLiteral,
        ),
        (
            "unit-p016-b0004",
            "[[GLYPHST_NUMBER_0000_88F291A0]]",
            "25",
            SemanticProtectedTokenKind::NumericLiteral,
        ),
        (
            "unit-p018-b0005",
            "[[GLYPHST_IDENTIFIER_0000_23D5135F]]",
            "AI",
            SemanticProtectedTokenKind::Identifier,
        ),
        (
            "unit-p018-b0006",
            "[[GLYPHST_IDENTIFIER_0000_23D5135F]]",
            "AI",
            SemanticProtectedTokenKind::Identifier,
        ),
        (
            "unit-p021-b0011",
            "[[GLYPHST_NUMBER_0000_88F291A0]]",
            "25",
            SemanticProtectedTokenKind::NumericLiteral,
        ),
    ];

    assert_eq!(fixtures.len(), 20);
    for (id, placeholder, original, kind) in fixtures {
        let unit = test_semantic_unit(
            id,
            &format!("Before {placeholder} after."),
            vec![test_semantic_token(placeholder, original, kind)],
        );
        let prepared = prepare_google_semantic_unit(&unit).unwrap();
        assert!(!prepared.html.contains("[[GLYPHST_"), "{id}");
        if matches!(
            kind,
            SemanticProtectedTokenKind::Formula
                | SemanticProtectedTokenKind::Citation
                | SemanticProtectedTokenKind::Url
        ) {
            assert!(prepared.html.contains("class=\"notranslate\""), "{id}");
        } else {
            assert!(!prepared.html.contains("class=\"notranslate\""), "{id}");
        }
        let translated_html = prepared.html.replacen("Before", "Translated", 1);
        assert_eq!(
            restore_google_semantic_html(&prepared, &translated_html, "batch HTML").unwrap(),
            unit.text.replacen("Before", "Translated", 1),
            "{id}"
        );
    }
}

#[test]
fn google_semantic_cache_identity_bypasses_pre_html_entries() {
    let settings = Settings::default();
    let target = LanguageCode::new("zh-hans").unwrap();
    let unit = test_semantic_unit(
        "unit-google-cache",
        "Use [[GLYPHST_IDENTIFIER_0000_AAAAAAAA]].",
        vec![test_semantic_token(
            "[[GLYPHST_IDENTIFIER_0000_AAAAAAAA]]",
            "AI",
            SemanticProtectedTokenKind::Identifier,
        )],
    );
    let key = semantic_cache_key(
        &TranslationExecutionConfig::from_settings(&settings),
        TranslationProvider::Google,
        None,
        &target,
        &unit,
    );
    let mut changed_token = unit.clone();
    changed_token.protected_tokens[0].original = "ML".to_string();

    assert!(key.starts_with("google-semantic-v4-"));
    assert_ne!(
        key,
        semantic_cache_key(
            &TranslationExecutionConfig::from_settings(&settings),
            TranslationProvider::Google,
            None,
            &target,
            &changed_token
        )
    );
    assert!(!key.starts_with("semantic-v3-"));
}

#[test]
fn google_inline_objects_are_reassembled_at_their_source_anchors() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let body = read_json_request(&mut stream);
        let queries = body["q"].as_array().unwrap();
        assert_eq!(queries.len(), 3);
        assert!(queries.iter().all(|query| {
            let query = query.as_str().unwrap();
            !query.contains("GLYPHST_INLINE_OBJECT")
                && !query.contains("book-icon")
                && !query.contains("pencil-icon")
                && !query.contains("computer-icon")
                && !query.contains("mail-icon")
        }));
        write_json_response(
            &mut stream,
            json!({
                "data": {
                    "translations": [
                        { "translatedText": "日记显示为" },
                        { "translatedText": "，而电子邮件显示为" },
                        {
                            "translatedText": "点击头像。<span translate=\"no\" class=\"notranslate\">style-end</span>"
                        }
                    ]
                }
            }),
        );
        body
    });

    let mut settings = Settings::default();
    let google = settings
        .translation
        .provider_config_mut(TranslationProvider::Google);
    google.api_key = "test-key".to_string();
    google.base_url = Some(format!("http://localhost:{port}"));
    let book = "[[GLYPHST_INLINE_OBJECT_0000_AAAAAAAA]]";
    let pencil = "[[GLYPHST_INLINE_OBJECT_0001_BBBBBBBB]]";
    let computer = "[[GLYPHST_INLINE_OBJECT_0002_CCCCCCCC]]";
    let mail = "[[GLYPHST_INLINE_OBJECT_0003_DDDDDDDD]]";
    let style_end = "[[GLYPHST_STYLE_END_0004_EEEEEEEE]]";
    let unit = test_semantic_unit(
        "unit-google-inline-anchors",
        &format!(
            "The diary appears as {book}{pencil}, while the email appears as \
             {computer}{mail}{style_end}. Click the portrait."
        ),
        vec![
            test_semantic_token(book, "book-icon", SemanticProtectedTokenKind::InlineObject),
            test_semantic_token(
                pencil,
                "pencil-icon",
                SemanticProtectedTokenKind::InlineObject,
            ),
            test_semantic_token(
                computer,
                "computer-icon",
                SemanticProtectedTokenKind::InlineObject,
            ),
            test_semantic_token(mail, "mail-icon", SemanticProtectedTokenKind::InlineObject),
            test_semantic_token(
                style_end,
                "style-end",
                SemanticProtectedTokenKind::StyleBoundary,
            ),
        ],
    );

    let translated = TranslationService::new()
        .unwrap()
        .translate_semantic_batch(
            &TranslationExecutionConfig::from_settings(&settings),
            TranslationProvider::Google,
            Some(LanguageCode::new("en").unwrap()),
            LanguageCode::new("zh-hans").unwrap(),
            vec![unit],
        )
        .unwrap();
    let request = server.join().unwrap();

    assert_eq!(request["format"], "html");
    assert_eq!(translated.len(), 1);
    assert_eq!(
        translated[0].text,
        format!(
            "日记显示为 {book}{pencil}，而电子邮件显示为 {computer}{mail}。点击头像。{style_end}"
        )
    );
    assert!(translated[0].validation_error.is_none());
}

#[test]
fn google_semantic_recovery_uses_batch_singleton_then_segmented_html() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for index in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let body = read_json_request(&mut stream);
            let queries = body["q"].as_array().unwrap();
            let translated = match index {
                0 => vec![
                    queries[0]
                        .as_str()
                        .unwrap()
                        .replace("First unit needs translation.", "Première unité traduite."),
                    queries[1]
                        .as_str()
                        .unwrap()
                        .replace("Second unit has", "Deuxième unité contient")
                        .replace("25", "vingt-cinq"),
                ],
                1 => vec![
                    queries[0]
                        .as_str()
                        .unwrap()
                        .replace("Second unit has", "Deuxième unité contient")
                        .replace("25", "vingt-cinq"),
                ],
                2 => vec!["Deuxième unité contient ".to_string(), " cas.".to_string()],
                _ => unreachable!(),
            };
            write_json_response(
                &mut stream,
                json!({
                    "data": {
                        "translations": translated
                            .into_iter()
                            .map(|translated_text| json!({
                                "translatedText": translated_text
                            }))
                            .collect::<Vec<_>>()
                    }
                }),
            );
            requests.push(body);
        }
        requests
    });

    let mut settings = Settings::default();
    let google = settings
        .translation
        .provider_config_mut(TranslationProvider::Google);
    google.api_key = "test-key".to_string();
    google.base_url = Some(format!("http://localhost:{port}"));
    let placeholder = "[[GLYPHST_NUMBER_0000_88F291A0]]";
    let units = vec![
        test_semantic_unit(
            "unit-google-valid-batch",
            "First unit needs translation.",
            Vec::new(),
        ),
        test_semantic_unit(
            "unit-google-recovery",
            &format!("Second unit has {placeholder} cases."),
            vec![test_semantic_token(
                placeholder,
                "25",
                SemanticProtectedTokenKind::NumericLiteral,
            )],
        ),
    ];

    let translated = TranslationService::new()
        .unwrap()
        .translate_semantic_batch(
            &TranslationExecutionConfig::from_settings(&settings),
            TranslationProvider::Google,
            None,
            LanguageCode::new("fr").unwrap(),
            units,
        )
        .unwrap();
    let requests = server.join().unwrap();

    assert_eq!(translated.len(), 2);
    assert_eq!(translated[0].text, "Première unité traduite.");
    assert_eq!(
        translated[1].text,
        format!("Deuxième unité contient {placeholder} cas.")
    );
    assert!(
        translated
            .iter()
            .all(|translation| !translation.cached && translation.validation_error.is_none())
    );
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| request["format"] == "html"));
    assert_eq!(requests[0]["q"].as_array().unwrap().len(), 2);
    assert_eq!(requests[1]["q"].as_array().unwrap().len(), 1);
    assert_eq!(requests[2]["q"].as_array().unwrap().len(), 2);
    let batch_invalid = requests[0]["q"][1].as_str().unwrap();
    let singleton = requests[1]["q"][0].as_str().unwrap();
    assert_eq!(batch_invalid, singleton);
    assert!(batch_invalid.contains("<span>25</span>"));
    assert!(!batch_invalid.contains("[[GLYPHST_"));
    assert!(
        requests[2]["q"]
            .as_array()
            .unwrap()
            .iter()
            .all(|query| !query.as_str().unwrap().contains("<span"))
    );
}

#[test]
fn cache_keys_do_not_include_secrets() {
    let request = TranslationRequest {
        provider: TranslationProvider::DeepL,
        source_language: None,
        target_language: LanguageCode::new("en").unwrap(),
        texts: vec!["hello".to_string()],
        context_before: None,
        context_after: None,
    };
    assert!(!cache_key(&request).contains("hello"));
}

#[test]
fn memory_cache_serves_translations_without_persistent_storage() {
    let mut cache = TranslationCache::memory();
    let value = CachedTranslation {
        translations: vec!["hello".to_string()],
        detected_source_language: Some("en".to_string()),
        updated_at: 1,
    };

    cache.insert("key", value).unwrap();

    assert_eq!(
        cache.get("key").map(|cached| cached.translations),
        Some(vec!["hello".to_string()])
    );
}

#[test]
fn clear_removes_memory_and_persistent_cache_entries() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("glyphst-translation-cache-{nonce:x}"));
    let path = directory.join("translations-v2.redb");
    let mut cache = TranslationCache::open_at(&path).unwrap();
    let cached = CachedTranslation {
        translations: vec!["bonjour".to_string()],
        detected_source_language: Some("en".to_string()),
        updated_at: 1,
    };

    cache.insert("before-clear", cached.clone()).unwrap();
    assert!(cache.get("before-clear").is_some());
    cache.clear().unwrap();
    assert!(cache.get("before-clear").is_none());

    cache.insert("after-clear", cached).unwrap();
    assert!(cache.get("after-clear").is_some());
    drop(cache);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn semantic_cache_keys_separate_protocol_context_model_and_endpoint() {
    let settings = Settings::default();
    let provider = TranslationProvider::Ai;
    let target = LanguageCode::new("fr").unwrap();
    let unit = SemanticTranslationUnit {
        id: "unit-1".to_string(),
        role: "body".to_string(),
        text: "Hello [[GLYPHST_NUMBER_0000_DEADBEEF]]".to_string(),
        protected_tokens: Vec::new(),
        context_before: Some("Before".to_string()),
        context_after: Some("After".to_string()),
    };
    let base_key = semantic_cache_key(
        &TranslationExecutionConfig::from_settings(&settings),
        provider,
        None,
        &target,
        &unit,
    );

    let mut changed_context = unit.clone();
    changed_context.context_before = Some("Different".to_string());
    assert_ne!(
        base_key,
        semantic_cache_key(
            &TranslationExecutionConfig::from_settings(&settings),
            provider,
            None,
            &target,
            &changed_context,
        )
    );

    let mut changed_model = settings.clone();
    let ai_provider = changed_model.ai.resolved_translation().provider;
    changed_model
        .ai
        .provider_config_mut(ai_provider)
        .ensure_model(AiModel::new("other-model", "Other model", "Test"));
    changed_model.ai.translation_model = Some(ModelRef::new(ai_provider, "other-model"));
    assert_ne!(
        base_key,
        semantic_cache_key(
            &TranslationExecutionConfig::from_settings(&changed_model),
            provider,
            None,
            &target,
            &unit,
        )
    );

    let mut endpoint_base = settings.clone();
    let custom_provider = endpoint_base.ai.add_custom_provider(
        "Private endpoint",
        AiApiStyle::OpenAi,
        "https://translation-a.example.test/v1",
        "",
    );
    {
        let config = endpoint_base.ai.provider_config_mut(custom_provider);
        config.enabled = true;
        config.ensure_model(AiModel::new("private-model", "Private model", "Test"));
    }
    endpoint_base.ai.translation_model = Some(ModelRef::new(custom_provider, "private-model"));
    let endpoint_key = semantic_cache_key(
        &TranslationExecutionConfig::from_settings(&endpoint_base),
        provider,
        None,
        &target,
        &unit,
    );

    let mut changed_endpoint = endpoint_base.clone();
    changed_endpoint
        .ai
        .provider_config_mut(custom_provider)
        .base_url = Some("https://translation-b.example.test/v1/".to_string());
    assert_ne!(
        endpoint_key,
        semantic_cache_key(
            &TranslationExecutionConfig::from_settings(&changed_endpoint),
            provider,
            None,
            &target,
            &unit,
        )
    );
    assert!(base_key.starts_with("semantic-v5-"));
}

#[test]
fn semantic_cache_accepts_only_placeholder_validated_items() {
    let service = TranslationService::new().unwrap();
    let settings = Settings::default();
    let provider = TranslationProvider::Ai;
    let target = LanguageCode::new("fr").unwrap();
    let unit = SemanticTranslationUnit {
        id: "unit-1".to_string(),
        role: "body".to_string(),
        text: "Value [[GLYPHST_NUMBER_0000_DEADBEEF]]".to_string(),
        protected_tokens: Vec::new(),
        context_before: None,
        context_after: None,
    };
    let config = TranslationExecutionConfig::from_settings(&settings);
    let key = semantic_cache_key(&config, provider, None, &target, &unit);

    service.cache_validated_semantic_translation(&config, provider, None, &target, &unit, "Valeur");
    assert!(service.cache.lock().unwrap().get(&key).is_none());

    service.cache_validated_semantic_translation(
        &config,
        provider,
        None,
        &target,
        &unit,
        "Valeur [[GLYPHST_NUMBER_0000_DEADBEEF]]",
    );
    assert_eq!(
        service
            .cache
            .lock()
            .unwrap()
            .get(&key)
            .unwrap()
            .translations,
        ["Valeur [[GLYPHST_NUMBER_0000_DEADBEEF]]"]
    );
}

#[test]
fn poisoned_legacy_semantic_cache_is_bypassed() {
    let service = TranslationService::new().unwrap();
    service
        .cache
        .lock()
        .unwrap()
        .insert(
            "semantic-v1-poisoned",
            CachedTranslation {
                translations: vec![String::new()],
                detected_source_language: None,
                updated_at: 1,
            },
        )
        .unwrap();
    let settings = Settings::default();
    let unit = SemanticTranslationUnit {
        id: "unit-1".to_string(),
        role: "body".to_string(),
        text: "Hello".to_string(),
        protected_tokens: Vec::new(),
        context_before: None,
        context_after: None,
    };
    let key = semantic_cache_key(
        &TranslationExecutionConfig::from_settings(&settings),
        TranslationProvider::Ai,
        None,
        &LanguageCode::new("fr").unwrap(),
        &unit,
    );

    assert!(service.cache.lock().unwrap().get(&key).is_none());
    assert!(!key.starts_with("semantic-v1-"));
}

fn test_semantic_unit(
    id: &str,
    text: &str,
    protected_tokens: Vec<SemanticProtectedToken>,
) -> SemanticTranslationUnit {
    SemanticTranslationUnit {
        id: id.to_string(),
        role: "body".to_string(),
        text: text.to_string(),
        protected_tokens,
        context_before: None,
        context_after: None,
    }
}

fn test_semantic_token(
    placeholder: &str,
    original: &str,
    kind: SemanticProtectedTokenKind,
) -> SemanticProtectedToken {
    SemanticProtectedToken {
        placeholder: placeholder.to_string(),
        original: original.to_string(),
        kind,
    }
}

fn read_json_request(stream: &mut TcpStream) -> Value {
    let mut request = Vec::new();
    let header_end = loop {
        let mut byte = [0; 1];
        stream.read_exact(&mut byte).unwrap();
        request.push(byte[0]);
        if request.ends_with(b"\r\n\r\n") {
            break request.len();
        }
    };
    let headers = std::str::from_utf8(&request[..header_end]).unwrap();
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
        })
        .unwrap();
    let mut body = vec![0; content_length];
    stream.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn write_json_response(stream: &mut TcpStream, value: Value) {
    let body = serde_json::to_vec(&value).unwrap();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    stream.flush().unwrap();
}
