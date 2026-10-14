pub mod app_paths;
pub mod localize;
pub mod settings;
pub mod theme;

pub use settings::{
    AcpAgentConfig, AiApiStyle, AiCapabilityOverride, AiCapabilitySupport, AiModel,
    AiModelCapabilities, AiProvider, AiProviderConfig, AiSettings, AiThinkingLevel,
    AnthropicThinking, AppLanguage, BehaviorSettings, CopyFormat, DEFAULT_OCR_ENDPOINT,
    DEFAULT_OCR_MODEL, DEFAULT_PADDLE_OCR_ENDPOINT, DEFAULT_PADDLE_OCR_MODEL,
    DEFAULT_PAGE_IMAGE_LIMIT, DEFAULT_SMOOTH_SCROLLING, DEFAULT_UI_FONT_FAMILY,
    DEFAULT_UI_FONT_SIZE, DEFAULT_UI_FONT_WEIGHT, DEFAULT_UNLIMITED_OCR_ENDPOINT, DEFAULT_ZOOM,
    DEFAULT_ZOOM_STEP, DocumentProgressDisplay, DocumentTranslationMode, DocumentViewSettings,
    FontSettings, KeyBindingOverride, KeymapPreset, ModelLimits, ModelRef, NewTabSettings,
    NewTabViewMode, OcrMode, OcrProvider, OcrSettings, PaddleOcrSettings, PageImageQuality,
    PageTextFormat, PathChooserBackend, PdfContentReviewMode, PdfContentReviewSettings,
    PinnedTranslationSettings, ProviderSecretId, ProviderSecretPersistence, ProviderSecretStatus,
    QuickAskPosition, QuickAskSettings, ResolvedAiConfig, RestoreOnStartup, Settings,
    TabBarVisibility, TranslationBoxCorner, TranslationProvider, TranslationProviderConfig,
    TranslationSettings, TranslationTargetLanguage, UnlimitedOcrSettings, WindowBackgroundMode,
    WindowSettings, keychain_secret_ids, load_user_settings, parse_settings, read_keychain_secrets,
    user_themes_dir,
};
pub use settings::{
    CredentialStore, MemoryCredentials, SystemKeychain, config_owned_secret_ids,
    save_user_settings, save_user_settings_to,
};
