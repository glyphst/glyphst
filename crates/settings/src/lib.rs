pub mod app_paths;
pub mod localize;
pub mod settings;
pub mod theme;

pub use settings::{
    AiApiStyle, AiCapabilityOverride, AiCapabilitySupport, AiModel, AiModelCapabilities,
    AiProvider, AiProviderConfig, AiSettings, AiThinkingLevel, AppLanguage, BehaviorSettings,
    CopyFormat, DEFAULT_OCR_ENDPOINT, DEFAULT_OCR_MODEL, DEFAULT_PADDLE_OCR_ENDPOINT,
    DEFAULT_PADDLE_OCR_MODEL, DEFAULT_PAGE_IMAGE_LIMIT, DEFAULT_SMOOTH_SCROLLING,
    DEFAULT_UI_FONT_FAMILY, DEFAULT_UI_FONT_SIZE, DEFAULT_UI_FONT_WEIGHT,
    DEFAULT_UNLIMITED_OCR_ENDPOINT, DEFAULT_ZOOM, DEFAULT_ZOOM_STEP, DocumentTranslationMode,
    FontSettings, KeyBindingOverride, KeymapPreset, ModelRef, NewTabSettings, NewTabViewMode,
    OcrMode, OcrProvider, OcrSettings, PaddleOcrSettings, PageImageQuality, PageTextFormat,
    PdfContentReviewMode, PdfContentReviewSettings, PinnedTranslationSettings, QuickAskSettings,
    ResolvedAiConfig, RestoreOnStartup, Settings, SettingsStore, TabBarVisibility,
    TranslationBoxCorner, TranslationProvider, TranslationProviderConfig, TranslationSettings,
    UnlimitedOcrSettings, WindowBackgroundMode, WindowSettings, parse_settings, setup_ui_font,
    sync_component_theme, ui_rems, user_themes_dir,
};
