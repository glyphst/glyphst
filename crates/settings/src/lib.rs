pub mod app_paths;
pub mod localize;
pub mod settings;
mod values;

pub use settings::{
    AcpAgentConfig, AiApiStyle, AiCapabilityOverride, AiCapabilitySupport, AiModel,
    AiModelCapabilities, AiProvider, AiProviderConfig, AiSettings, AiThinkingLevel,
    AnthropicThinking, DEFAULT_LOCAL_OCR_MODEL, DEFAULT_OCR_ENDPOINT, DEFAULT_OCR_MODEL,
    DEFAULT_PADDLE_OCR_ENDPOINT, DEFAULT_PADDLE_OCR_MODEL, DEFAULT_PAGE_IMAGE_LIMIT,
    DEFAULT_UNLIMITED_OCR_ENDPOINT, DocumentTranslationMode, LocalOcrSettings, ModelLimits,
    ModelRef, OcrDevice, OcrMode, OcrProvider, OcrSettings, PaddleOcrSettings, PageImageQuality,
    PageTextFormat, PdfContentReviewMode, PdfContentReviewSettings, PinnedTranslationSettings,
    ProviderSecretId, ProviderSecretPersistence, ProviderSecretStatus, QuickAskPosition,
    QuickAskSettings, ResolvedAiConfig, Settings, TranslationBoxCorner, TranslationProvider,
    TranslationProviderConfig, TranslationSettings, TranslationTargetLanguage,
    UnlimitedOcrSettings, WindowBackgroundMode, WindowSettings, keychain_secret_ids,
    load_user_settings, parse_settings, read_keychain_secrets, settings_file,
};
pub use settings::{
    CredentialStore, MemoryCredentials, SystemKeychain, config_owned_secret_ids,
    save_user_settings, save_user_settings_to,
};
