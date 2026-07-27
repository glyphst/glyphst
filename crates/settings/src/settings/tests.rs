use super::persistence::*;
use super::*;
use crate::theme::{ThemeAppearanceMode, ThemeOrigin};
use gpui::TestAppContext;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn parses_partial_settings_with_defaults() {
    let settings = parse_settings(
        r##"
theme {
    mode "dark"
    light "Default Light"
    dark "Default Dark"
}
zoom {
    default 1.5
}
window {
    opacity 0.85
    appearance "transparent"
}
theme-overrides {
    colors {
        background "#101010"
    }
}
"##,
    )
    .unwrap();

    assert_eq!(settings.behavior.default_zoom, 1.5);
    assert_eq!(settings.behavior.zoom_step, DEFAULT_ZOOM_STEP);
    assert_eq!(settings.behavior.smooth_scrolling, DEFAULT_SMOOTH_SCROLLING);
    assert!(!settings.behavior.dim_inactive_panes);
    assert_eq!(settings.window.opacity, 0.85);
    assert_eq!(
        settings.window.background,
        WindowBackgroundMode::Transparent
    );
    assert_eq!(
        settings.theme,
        ThemeSelection {
            mode: ThemeAppearanceMode::Dark,
            light: DEFAULT_LIGHT_THEME_ID.to_string(),
            dark: DEFAULT_DARK_THEME_ID.to_string(),
        }
    );
    assert_eq!(settings.overrides.background, Some(0x101010));
    assert_eq!(settings.overrides.selection_highlight, None);
}

#[test]
fn parses_and_exports_epub_reading_settings() {
    let settings = parse_settings(
        r#"
reading {
    font_size 20.0
    line_height 1.8
    horizontal_margin 48.0
    max_content_width 680.0
    theme "sepia"
    publisher_styles #false
}
"#,
    )
    .unwrap();

    assert_eq!(settings.reading.font_size, 20.0);
    assert_eq!(settings.reading.line_height, 1.8);
    assert_eq!(settings.reading.horizontal_margin, 48.0);
    assert_eq!(settings.reading.max_content_width, 680.0);
    assert_eq!(settings.reading.theme, EpubTheme::Sepia);
    assert!(!settings.reading.use_publisher_styles);
    assert_eq!(
        parse_settings(&settings.to_kdl_string()).unwrap().reading,
        settings.reading
    );
}

#[test]
fn clamps_window_opacity() {
    let settings = parse_settings(
        r##"
window {
    opacity 0.1
    appearance "blurred"
}
"##,
    )
    .unwrap();

    assert_eq!(settings.window.opacity, MIN_WINDOW_OPACITY);
    assert_eq!(settings.window.background, WindowBackgroundMode::Blurred);
}

#[test]
fn exported_settings_are_reparseable() {
    let settings = Settings {
        behavior: BehaviorSettings {
            default_zoom: 1.5,
            zoom_step: 1.2,
            smooth_scrolling: false,
            dim_inactive_panes: true,
            copy_format: CopyFormat::Markdown,
            semantic_copy: false,
        },
        theme: ThemeSelection {
            mode: ThemeAppearanceMode::Dark,
            ..ThemeSelection::default()
        },
        overrides: PartialTheme {
            background: Some(0x181818),
            selection_highlight: Some(0x61afef66),
            page_gap: Some(32.0),
            ..PartialTheme::default()
        },
        ..Settings::default()
    };

    let exported = settings.clone().to_kdl_string();
    assert_eq!(parse_settings(&exported).unwrap(), settings);
}

#[test]
fn parses_and_exports_keymap_settings() {
    let settings = parse_settings(
        r##"
keymap "vim"
keybindings {
    next-page "j"
    jump-backward "ctrl-o"
}
"##,
    )
    .unwrap();

    assert_eq!(settings.keymap_preset, KeymapPreset::Vim);
    assert_eq!(
        settings.keybinding_overrides,
        vec![
            KeyBindingOverride {
                action: "next-page".to_string(),
                keystrokes: "j".to_string(),
                context: None,
            },
            KeyBindingOverride {
                action: "jump-backward".to_string(),
                keystrokes: "ctrl-o".to_string(),
                context: None,
            },
        ]
    );

    assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
}

#[test]
fn parses_and_exports_language_settings() {
    let settings = parse_settings(
        r##"
language "zh-CN"
"##,
    )
    .unwrap();

    assert_eq!(settings.language, AppLanguage::SimplifiedChinese);
    assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
}

#[test]
fn parses_and_exports_contextual_keybindings() {
    let settings = parse_settings(
        r##"
keybindings {
    next-search-result keys="ctrl-g" context="search_results"
    next-page keys="" context="document_view"
}
"##,
    )
    .unwrap();

    assert_eq!(
        settings.keybinding_overrides,
        vec![
            KeyBindingOverride {
                action: "next-search-result".to_string(),
                keystrokes: "ctrl-g".to_string(),
                context: Some("search_results".to_string()),
            },
            KeyBindingOverride {
                action: "next-page".to_string(),
                keystrokes: String::new(),
                context: Some("document_view".to_string()),
            },
        ]
    );
    assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
}

#[test]
fn parses_and_exports_tab_settings() {
    let settings = parse_settings(
        r##"
tabs {
    visibility "always"
}
"##,
    )
    .unwrap();

    assert_eq!(settings.tab_bar_visibility, TabBarVisibility::Always);
    assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
}

#[test]
fn parses_and_exports_font_settings() {
    let settings = parse_settings(
        r##"
font {
    ui_font_family "Noto Sans CJK SC"
    ui_font_size 14
    ui_font_weight 500
    ui_font_fallbacks "Noto Sans CJK SC" "Source Han Sans SC" "Noto Sans CJK SC" ""
}
"##,
    )
    .unwrap();

    assert_eq!(settings.font.ui_font_family, "Noto Sans CJK SC");
    assert_eq!(settings.font.ui_font_size, 14.0);
    assert_eq!(settings.font.ui_font_weight, 500.0);
    assert_eq!(
        settings.font.ui_font_fallbacks,
        vec!["Noto Sans CJK SC", "Source Han Sans SC"]
    );
    assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
}

#[test]
fn clamps_font_settings() {
    let settings = parse_settings(
        r##"
font {
    ui_font_family ""
    ui_font_size 1000
    ui_font_weight 10
}
"##,
    )
    .unwrap();

    assert_eq!(settings.font.ui_font_family, DEFAULT_UI_FONT_FAMILY);
    assert_eq!(settings.font.ui_font_size, MAX_UI_FONT_SIZE);
    assert_eq!(settings.font.ui_font_weight, MIN_UI_FONT_WEIGHT);
}

#[test]
fn migrates_legacy_ai_settings() {
    let settings = parse_settings(
        r##"
ai {
    provider "anthropic"
    model "claude-3-5-sonnet-latest"
    api_key "test-key"
    base_url "https://api.example.com/"
}
"##,
    )
    .unwrap();

    assert_eq!(
        settings.ai.main_model,
        Some(ModelRef::new(
            AiProvider::Anthropic,
            "claude-3-5-sonnet-latest"
        ))
    );
    let config = settings.ai.provider_config(AiProvider::Anthropic);
    assert!(config.enabled);
    assert!(config.model("claude-3-5-sonnet-latest").is_some());
    assert_eq!(config.api_key, "test-key");
    assert_eq!(config.base_url, Some("https://api.example.com".to_string()));
    assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
}

#[test]
fn parses_and_exports_per_provider_ai_settings() {
    let settings = parse_settings(
        r##"
ai {
    provider "deepseek"
    providers {
        deepseek {
            model "deepseek-chat"
            api_key "deepseek-key"
            base_url "https://api.deepseek.com/v1/"
        }
        openrouter {
            model "openai/gpt-4o-mini"
            api_key "openrouter-key"
        }
    }
}
"##,
    )
    .unwrap();

    assert_eq!(
        settings.ai.main_model,
        Some(ModelRef::new(
            AiProvider::DeepSeek,
            AiProvider::DeepSeek.default_model()
        ))
    );
    assert_eq!(settings.ai.translation_model, settings.ai.main_model);
    assert_eq!(settings.ai.thinking_level, AiThinkingLevel::Medium);
    let deepseek = settings.ai.provider_config(AiProvider::DeepSeek);
    assert!(deepseek.enabled);
    assert!(deepseek.model("deepseek-chat").is_some());
    assert_eq!(deepseek.api_key, "deepseek-key");
    assert_eq!(
        deepseek.base_url,
        Some("https://api.deepseek.com/v1".to_string())
    );
    let openrouter = settings.ai.provider_config(AiProvider::OpenRouter);
    assert!(openrouter.model("openai/gpt-4o-mini").is_some());
    assert_eq!(openrouter.api_key, "openrouter-key");
    assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
}

#[test]
fn parses_and_exports_ai_thinking_level() {
    let settings = parse_settings(
        r##"
ai {
    provider "openai"
    thinking "high"
}
"##,
    )
    .unwrap();

    assert_eq!(settings.ai.thinking_level, AiThinkingLevel::High);
    assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
}

#[test]
fn copy_settings_use_semantic_default_and_reject_invalid_format() {
    let settings = parse_settings(
        r##"
copy {
    format "invalid"
}
"##,
    )
    .unwrap();

    assert_eq!(settings.behavior.copy_format, CopyFormat::PlainText);
    assert!(settings.behavior.semantic_copy);
}

#[test]
fn parses_and_exports_copy_settings() {
    let settings = parse_settings(
        r##"
copy {
    format "markdown"
    semantic #false
}
"##,
    )
    .unwrap();

    assert_eq!(settings.behavior.copy_format, CopyFormat::Markdown);
    assert!(!settings.behavior.semantic_copy);
    assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
}

#[test]
fn page_context_settings_use_expected_defaults_and_presets() {
    let settings = parse_settings(
        r##"
ai {
    page_text_format "invalid"
    page_image_quality "invalid"
    page_image_limit 0
}
"##,
    )
    .unwrap();

    assert_eq!(settings.ai.page_text_format, PageTextFormat::Block);
    assert_eq!(settings.ai.page_image_quality, PageImageQuality::Medium);
    assert_eq!(settings.ai.page_image_limit, 1);
    assert_eq!(PageImageQuality::Low.max_dimension(), 1024);
    assert_eq!(PageImageQuality::Low.jpeg_quality(), 65);
    assert_eq!(PageImageQuality::Medium.max_dimension(), 1600);
    assert_eq!(PageImageQuality::Medium.jpeg_quality(), 75);
    assert_eq!(PageImageQuality::High.max_dimension(), 2400);
    assert_eq!(PageImageQuality::High.jpeg_quality(), 85);
}

#[test]
fn parses_and_exports_ai_page_context_settings() {
    let settings = parse_settings(
        r##"
ai {
    page_text_format "semantic"
    page_image_quality "high"
    page_image_limit 128
}
"##,
    )
    .unwrap();

    assert_eq!(settings.ai.page_text_format, PageTextFormat::Semantic);
    assert_eq!(settings.ai.page_image_quality, PageImageQuality::High);
    assert_eq!(settings.ai.page_image_limit, 128);
    assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
}

#[test]
fn page_image_limit_defaults_to_eight_without_an_upper_clamp() {
    assert_eq!(
        parse_settings("").unwrap().ai.page_image_limit,
        DEFAULT_PAGE_IMAGE_LIMIT
    );

    let settings = parse_settings(
        r##"
ai {
    page_image_limit 1000000
}
"##,
    )
    .unwrap();
    assert_eq!(settings.ai.page_image_limit, 1_000_000);
    assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
}

#[test]
fn parses_exports_and_resolves_translation_model() {
    let settings = parse_settings(
        r##"
ai {
    thinking "high"
    translation_model "anthropic" "claude-3-5-haiku-latest"
    providers {
        anthropic {
            enabled #true
            api_key "translation-key"
            base_url "https://translation.example.com/"
        }
    }
}
"##,
    )
    .unwrap();

    assert_eq!(
        settings.ai.translation_model,
        Some(ModelRef::new(
            AiProvider::Anthropic,
            "claude-3-5-haiku-latest"
        ))
    );
    let resolved = settings.ai.resolved_translation();
    assert_eq!(resolved.provider, AiProvider::Anthropic);
    assert_eq!(resolved.model, "claude-3-5-haiku-latest");
    assert_eq!(resolved.api_key, "translation-key");
    assert_eq!(resolved.base_url, "https://translation.example.com");
    assert_eq!(resolved.thinking_level, AiThinkingLevel::Off);
    assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
}

#[test]
fn missing_translation_model_falls_back_to_an_enabled_model() {
    let mut settings = Settings::default();
    settings.ai.translation_model = None;
    settings.sanitize();

    assert_eq!(settings.ai.translation_model, settings.ai.main_model);
    assert_eq!(
        settings.ai.resolved_translation().thinking_level,
        AiThinkingLevel::Off
    );
}

#[test]
fn parses_and_exports_quick_ask_settings() {
    let settings = parse_settings(
        r##"
ai {
    quick_ask {
        transparency 0.35
        text_size 18
    }
}
"##,
    )
    .unwrap();

    assert_eq!(settings.ai.quick_ask.transparency, 0.35);
    assert_eq!(settings.ai.quick_ask.text_size, 18.0);
    assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
}

#[test]
fn parses_and_exports_translation_settings() {
    let settings = parse_settings(
        r##"
translation {
    provider "microsoft"
    target_language "zh-Hans"
    pinned {
        enabled #true
        width 420
        corner "bottom-left"
    }
    providers {
        microsoft {
            enabled #true
            api_key "test-key"
            base_url "https://translator.example.com/"
            region "eastus"
        }
    }
}
"##,
    )
    .unwrap();

    assert_eq!(
        settings.translation.active_provider,
        TranslationProvider::Microsoft
    );
    assert_eq!(settings.translation.target_language, "zh-hans");
    assert_eq!(settings.translation.pinned.width, 420.0);
    assert_eq!(
        settings.translation.pinned.corner,
        TranslationBoxCorner::BottomLeft
    );
    let provider = settings
        .translation
        .provider_config(TranslationProvider::Microsoft);
    assert_eq!(
        provider.base_url.as_deref(),
        Some("https://translator.example.com")
    );
    assert_eq!(provider.region.as_deref(), Some("eastus"));
    let exported = settings.to_kdl_string();
    let exported_document = exported.parse::<KdlDocument>().unwrap();
    let exported_pinned = exported_document
        .get("translation")
        .and_then(KdlNode::children)
        .and_then(|translation| translation.get("pinned"))
        .and_then(KdlNode::children)
        .unwrap();
    assert!(exported_pinned.get("enabled").is_none());
    assert_eq!(parse_settings(&exported).unwrap(), settings);
}

#[test]
fn ocr_settings_use_expected_defaults() {
    let settings = parse_settings("").unwrap();

    assert_eq!(settings.ocr, OcrSettings::default());
    assert_eq!(settings.ocr.mode, OcrMode::Always);
    assert_eq!(settings.ocr.provider, OcrProvider::Unlimited);
    assert_eq!(settings.ocr.paddle.endpoint, DEFAULT_PADDLE_OCR_ENDPOINT);
    assert_eq!(settings.ocr.paddle.model, DEFAULT_PADDLE_OCR_MODEL);
    assert!(settings.ocr.paddle.token.is_empty());
    assert_eq!(
        settings.ocr.unlimited.endpoint,
        DEFAULT_UNLIMITED_OCR_ENDPOINT
    );
    assert!(settings.ocr.unlimited.api_key.is_empty());
    assert!(settings.ocr.unlimited.secret_key.is_empty());
    assert!(!settings.to_kdl_string().contains("\nocr "));
}

#[test]
fn parses_sanitizes_and_exports_ocr_settings() {
    let settings = parse_settings(
        r#"
ocr {
    mode "auto"
    provider "paddle"
    providers {
        paddle {
            token "  paddle-token  "
            endpoint " https://ocr.example.com/ "
            model " custom-model "
        }
        unlimited {
            api_key "  baidu-api-key  "
            secret_key "  baidu-secret-key  "
            endpoint " https://baidu-ocr.example.com/ "
        }
    }
}
"#,
    )
    .unwrap();

    assert_eq!(settings.ocr.mode, OcrMode::Auto);
    assert_eq!(settings.ocr.provider, OcrProvider::Paddle);
    assert_eq!(settings.ocr.paddle.token, "paddle-token");
    assert_eq!(settings.ocr.paddle.endpoint, "https://ocr.example.com");
    assert_eq!(settings.ocr.paddle.model, "custom-model");
    assert_eq!(settings.ocr.unlimited.api_key, "baidu-api-key");
    assert_eq!(settings.ocr.unlimited.secret_key, "baidu-secret-key");
    assert_eq!(
        settings.ocr.unlimited.endpoint,
        "https://baidu-ocr.example.com"
    );
    assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
}

#[test]
fn legacy_paddle_settings_remain_readable_without_changing_the_default_provider() {
    let settings = parse_settings(
        r#"
ocr {
    token "legacy-paddle-token"
    endpoint "https://legacy-paddle.example.com/"
    model "legacy-model"
}
"#,
    )
    .unwrap();

    assert_eq!(settings.ocr.provider, OcrProvider::Unlimited);
    assert_eq!(settings.ocr.paddle.token, "legacy-paddle-token");
    assert_eq!(
        settings.ocr.paddle.endpoint,
        "https://legacy-paddle.example.com"
    );
    assert_eq!(settings.ocr.paddle.model, "legacy-model");
}

#[test]
fn invalid_ocr_settings_fall_back_without_exposing_secrets_in_debug() {
    let settings = parse_settings(
        r#"
ocr {
    mode "sometimes"
    provider "unknown"
    providers {
        paddle {
            token "secret-paddle-token"
            endpoint " / "
            model " "
        }
        unlimited {
            api_key "secret-baidu-api-key"
            secret_key "secret-baidu-secret-key"
            endpoint " "
        }
    }
}
"#,
    )
    .unwrap();

    assert_eq!(settings.ocr.mode, OcrMode::Always);
    assert_eq!(settings.ocr.provider, OcrProvider::Unlimited);
    assert_eq!(settings.ocr.paddle.endpoint, DEFAULT_PADDLE_OCR_ENDPOINT);
    assert_eq!(settings.ocr.paddle.model, DEFAULT_PADDLE_OCR_MODEL);
    assert_eq!(
        settings.ocr.unlimited.endpoint,
        DEFAULT_UNLIMITED_OCR_ENDPOINT
    );
    assert!(!format!("{:?}", settings.ocr).contains("secret-paddle-token"));
    assert!(!format!("{:?}", settings.ocr).contains("secret-baidu-api-key"));
    assert!(!format!("{:?}", settings.ocr).contains("secret-baidu-secret-key"));
}

#[test]
fn pinned_translation_enabled_is_transient() {
    let settings = parse_settings(
        r##"
translation {
    pinned {
        enabled #true
    }
}
"##,
    )
    .unwrap();
    let exported = settings.to_kdl_string();
    assert!(!exported.contains("pinned"));
}

#[test]
fn clamps_quick_ask_settings() {
    let settings = parse_settings(
        r##"
ai {
    quick_ask {
        transparency 2
        text_size 100
    }
}
"##,
    )
    .unwrap();

    assert_eq!(
        settings.ai.quick_ask.transparency,
        MAX_QUICK_ASK_TRANSPARENCY
    );
    assert_eq!(settings.ai.quick_ask.text_size, MAX_QUICK_ASK_TEXT_SIZE);

    let settings = parse_settings(
        r##"
ai {
    quick_ask {
        transparency -1
        text_size 1
    }
}
"##,
    )
    .unwrap();

    assert_eq!(
        settings.ai.quick_ask.transparency,
        MIN_QUICK_ASK_TRANSPARENCY
    );
    assert_eq!(settings.ai.quick_ask.text_size, MIN_QUICK_ASK_TEXT_SIZE);
}

#[test]
fn migrates_legacy_ai_title_model_settings() {
    let settings = parse_settings(
        r##"
ai {
    title_model {
        provider "openai"
        model "gpt-4o-mini"
        api_key "title-key"
        base_url "https://api.example.com/v1/"
    }
}
"##,
    )
    .unwrap();

    let title_model = settings.ai.title_model.as_ref().unwrap();
    assert_eq!(title_model.provider, AiProvider::OpenAi);
    assert_eq!(title_model.id, "gpt-4o-mini");
    let config = settings.ai.provider_config(AiProvider::OpenAi);
    assert_eq!(config.api_key, "title-key");
    assert_eq!(
        config.base_url,
        Some("https://api.example.com/v1".to_string())
    );
    assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
}

#[test]
fn title_model_uses_provider_credentials() {
    let settings = parse_settings(
        r##"
ai {
    provider "openai"
    providers {
        openai {
            model "gpt-5"
            api_key "main-key"
            base_url "https://main.example.com"
        }
    }
    title_model {
        provider "openai"
        model "gpt-4o-mini"
        api_key "title-key"
        base_url "https://title.example.com"
    }
}
"##,
    )
    .unwrap();

    let main = settings.ai.resolved_main();
    let title = settings.ai.resolved_title().unwrap();
    assert_eq!(main.api_key, "main-key");
    assert_eq!(main.base_url, "https://main.example.com");
    assert_eq!(title.api_key, "main-key");
    assert_eq!(title.base_url, "https://main.example.com");
    assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
}

#[test]
fn thinking_levels_are_provider_specific() {
    assert!(
        AiThinkingLevel::supported_for(AiProvider::OpenAi, "gpt-5")
            .contains(&AiThinkingLevel::XHigh)
    );
    assert!(
        !AiThinkingLevel::supported_for(AiProvider::OpenAi, "gpt-5")
            .contains(&AiThinkingLevel::Max)
    );
    assert!(
        AiThinkingLevel::supported_for(AiProvider::Anthropic, "claude-sonnet-4-5-latest")
            .contains(&AiThinkingLevel::Max)
    );
    assert!(
        AiThinkingLevel::supported_for(AiProvider::OpenRouter, "openai/gpt-5.5")
            .contains(&AiThinkingLevel::XHigh)
    );
    assert!(
        !AiThinkingLevel::supported_for(AiProvider::OpenRouter, "openai/gpt-4o")
            .contains(&AiThinkingLevel::XHigh)
    );
    assert_eq!(
        AiThinkingLevel::Max.clamp_for(AiProvider::OpenAi, "gpt-5"),
        AiThinkingLevel::XHigh
    );
    assert_eq!(
        AiThinkingLevel::Max.clamp_for(AiProvider::OpenRouter, "openai/gpt-5.5"),
        AiThinkingLevel::XHigh
    );
}

#[test]
fn theme_resolution_layers_selected_theme_and_overrides() {
    let catalog = ThemeCatalog::load(Path::new("/path/that/does/not/exist"));
    let settings = Settings {
        theme: ThemeSelection {
            mode: ThemeAppearanceMode::Dark,
            ..ThemeSelection::default()
        },
        overrides: PartialTheme {
            background: Some(0x101010),
            page_gap: Some(48.0),
            ..PartialTheme::default()
        },
        ..Settings::default()
    };

    let theme = catalog.resolve(
        settings.theme.name(ThemeAppearance::Light),
        settings.overrides,
    );

    assert_eq!(theme.background, 0x101010);
    assert_eq!(theme.page_gap, 48.0);
}

#[test]
fn unknown_theme_falls_back_to_default() {
    let catalog = ThemeCatalog::load(Path::new("/path/that/does/not/exist"));
    let theme = catalog.resolve("missing", PartialTheme::default());

    assert_eq!(
        theme,
        catalog.resolve(DEFAULT_LIGHT_THEME_ID, PartialTheme::default())
    );
}

#[test]
fn bundles_the_full_gpui_component_catalog() {
    let catalog = ThemeCatalog::load(Path::new("/path/that/does/not/exist"));
    let metadata = catalog.metadata();

    assert_eq!(metadata.len(), 39);
    assert!(metadata.iter().any(|theme| theme.id == "Catppuccin Latte"));
    assert!(metadata.iter().any(|theme| theme.id == "Catppuccin Mocha"));
    assert!(
        metadata
            .iter()
            .all(|theme| theme.origin == ThemeOrigin::Bundled)
    );
}

#[gpui::test]
fn installs_and_activates_the_catalog_in_gpui_components(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        let mut store = SettingsStore::memory();
        store.settings.theme.mode = ThemeAppearanceMode::Dark;
        store.settings.theme.dark = "Catppuccin Mocha".to_string();
        store.resolved_theme = store
            .theme_catalog
            .resolve("Catppuccin Mocha", store.settings.overrides);
        store.install_theme_catalog(cx).unwrap();
        cx.set_global(store);

        sync_component_theme(cx);

        assert_eq!(gpui_component::ThemeRegistry::global(cx).themes().len(), 39);
        assert_eq!(
            gpui_component::Theme::global(cx).theme_name().as_ref(),
            "Catppuccin Mocha"
        );
        assert!(gpui_component::Theme::global(cx).is_dark());
    });
}

#[test]
fn loads_user_theme_from_themes_directory() {
    let root = unique_test_dir();
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("solar.json"),
        r##"
{
  "name": "Solar test",
  "themes": [
    {
      "name": "Solar",
      "mode": "light",
      "colors": {
        "background": "#123456",
        "primary.background": "#abcdef"
      }
    }
  ]
}
"##,
    )
    .unwrap();

    let catalog = ThemeCatalog::load(&root);
    let theme = catalog.resolve("Solar", PartialTheme::default());
    let metadata = catalog
        .metadata()
        .into_iter()
        .find(|metadata| metadata.id == "Solar")
        .unwrap();

    assert_eq!(theme.background, 0x123456);
    assert_eq!(metadata.name, "Solar");
    assert_eq!(metadata.appearance.as_str(), "light");
    assert_eq!(metadata.origin, ThemeOrigin::User(root.join("solar.json")));

    fs::remove_dir_all(root).ok();
}

#[test]
fn unmanaged_settings_persist_to_state_without_creating_config() {
    let root = unique_test_dir();
    let config_path = root
        .join("config")
        .join("glyphst-reader")
        .join("config.kdl");
    let state_path = root.join("state").join("glyphst-reader").join("config.kdl");
    let settings = Settings {
        behavior: BehaviorSettings {
            default_zoom: 1.5,
            ..BehaviorSettings::default()
        },
        ..Settings::default()
    };
    let mut store = test_store(
        Settings::default(),
        false,
        config_path.clone(),
        state_path.clone(),
    );

    store.set_settings(settings.clone()).unwrap();

    assert!(!config_path.exists());
    assert_eq!(
        parse_settings(&fs::read_to_string(&state_path).unwrap()).unwrap(),
        settings
    );

    fs::remove_dir_all(root).ok();
}

#[test]
fn managed_settings_persist_to_config_without_touching_state_file() {
    let root = unique_test_dir();
    let config_path = root
        .join("config")
        .join("glyphst-reader")
        .join("config.kdl");
    let state_path = root.join("state").join("glyphst-reader").join("config.kdl");
    let settings = Settings {
        overrides: PartialTheme {
            page_gap: Some(48.0),
            ..PartialTheme::default()
        },
        ..Settings::default()
    };
    let mut store = test_store(
        Settings::default(),
        true,
        config_path.clone(),
        state_path.clone(),
    );

    store.set_settings(settings.clone()).unwrap();

    assert!(!state_path.exists());
    assert_eq!(
        parse_settings(&fs::read_to_string(config_path).unwrap()).unwrap(),
        settings
    );
    assert_eq!(store.settings, settings);

    fs::remove_dir_all(root).ok();
}

#[test]
fn failed_persistence_preserves_live_settings() {
    let root = unique_test_dir();
    let blocked_parent = root.join("not-a-directory");
    fs::create_dir_all(&root).unwrap();
    fs::write(&blocked_parent, "blocked").unwrap();
    let mut store = test_store(
        Settings::default(),
        false,
        root.join("config.kdl"),
        blocked_parent.join("config.kdl"),
    );
    let settings = Settings {
        behavior: BehaviorSettings {
            default_zoom: 1.5,
            ..BehaviorSettings::default()
        },
        ..Settings::default()
    };

    assert!(store.set_settings(settings).is_err());
    assert_eq!(store.settings, Settings::default());

    fs::remove_dir_all(root).ok();
}

#[cfg(unix)]
#[test]
fn persisted_settings_are_private() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = unique_test_dir();
    let state_path = root.join("state").join("glyphst-reader").join("config.kdl");
    let mut store = test_store(
        Settings::default(),
        false,
        root.join("config")
            .join("glyphst-reader")
            .join("config.kdl"),
        state_path.clone(),
    );

    store.set_settings(Settings::default()).unwrap();

    assert_eq!(
        fs::metadata(&state_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(state_path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    fs::remove_dir_all(root).ok();
}

#[test]
fn export_to_writes_selected_path() {
    let root = unique_test_dir();
    let export_path = root.join("picked").join("glyphst.kdl");
    let settings = Settings {
        behavior: BehaviorSettings {
            default_zoom: 1.5,
            zoom_step: 1.2,
            smooth_scrolling: false,
            dim_inactive_panes: true,
            copy_format: CopyFormat::PlainText,
            semantic_copy: false,
        },
        theme: ThemeSelection::default(),
        overrides: PartialTheme {
            background: Some(0x181818),
            selection_highlight: Some(0x61afef66),
            ..PartialTheme::default()
        },
        ..Settings::default()
    };
    let store = test_store(
        settings.clone(),
        false,
        root.join("config.kdl"),
        root.join("state.kdl"),
    );

    store.export_to(&export_path).unwrap();

    assert_eq!(
        parse_settings(&fs::read_to_string(&export_path).unwrap()).unwrap(),
        settings
    );

    fs::remove_dir_all(root).ok();
}

#[test]
fn parses_theme_selection() {
    let settings = parse_settings(
        r##"
theme {
    mode "system"
    light "Ayu Light"
    dark "Ayu Dark"
}
"##,
    )
    .unwrap();

    assert_eq!(
        settings.theme,
        ThemeSelection {
            mode: ThemeAppearanceMode::System,
            light: "Ayu Light".to_string(),
            dark: "Ayu Dark".to_string(),
        }
    );
}

#[test]
fn theme_selection_round_trips() {
    let settings = Settings {
        theme: ThemeSelection {
            mode: ThemeAppearanceMode::Dark,
            light: "Ayu Light".to_string(),
            dark: "Ayu Dark".to_string(),
        },
        ..Settings::default()
    };

    let exported = settings.to_kdl_string();
    assert_eq!(parse_settings(&exported).unwrap(), settings);
}

#[test]
fn set_mode_updates_mode_without_changing_pair() {
    let mut selection = ThemeSelection {
        mode: ThemeAppearanceMode::System,
        light: "Ayu Light".to_string(),
        dark: "Ayu Dark".to_string(),
    };
    selection.set_mode(ThemeAppearanceMode::Dark);

    assert_eq!(
        selection,
        ThemeSelection {
            mode: ThemeAppearanceMode::Dark,
            light: "Ayu Light".to_string(),
            dark: "Ayu Dark".to_string(),
        }
    );
}

#[test]
fn set_theme_updates_the_selected_appearance() {
    let mut selection = ThemeSelection {
        mode: ThemeAppearanceMode::System,
        light: "Ayu Light".to_string(),
        dark: "Ayu Dark".to_string(),
    };
    selection.set_theme(
        "Catppuccin Mocha".to_string(),
        ThemeAppearance::Dark,
        ThemeAppearance::Light,
    );

    assert_eq!(
        selection,
        ThemeSelection {
            mode: ThemeAppearanceMode::Dark,
            light: "Ayu Light".to_string(),
            dark: "Catppuccin Mocha".to_string(),
        }
    );
}

#[test]
fn theme_selection_name_returns_correct_theme() {
    let dynamic = ThemeSelection {
        mode: ThemeAppearanceMode::System,
        light: "Ayu Light".to_string(),
        dark: "Ayu Dark".to_string(),
    };

    assert_eq!(dynamic.name(ThemeAppearance::Light), "Ayu Light");
    assert_eq!(dynamic.name(ThemeAppearance::Dark), "Ayu Dark");

    let light_mode = ThemeSelection {
        mode: ThemeAppearanceMode::Light,
        light: "Ayu Light".to_string(),
        dark: "Ayu Dark".to_string(),
    };
    assert_eq!(light_mode.name(ThemeAppearance::Dark), "Ayu Light");
}

fn test_store(
    settings: Settings,
    user_managed: bool,
    config_path: PathBuf,
    state_path: PathBuf,
) -> SettingsStore {
    let theme_catalog = ThemeCatalog::load(Path::new("/path/that/does/not/exist"));
    let system_appearance = ThemeAppearance::Light;
    let resolved_theme =
        theme_catalog.resolve(settings.theme.name(system_appearance), settings.overrides);
    SettingsStore {
        settings,
        theme_catalog,
        resolved_theme,
        system_appearance,
        user_managed,
        config_path,
        state_path,
    }
}

fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "glyphst-settings-test-{}-{nanos}",
        std::process::id()
    ))
}
