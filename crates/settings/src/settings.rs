use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use std::{fs::OpenOptions, io::Write as _};

use anyhow::{Context as _, Result};
use kdl::{KdlDocument, KdlNode, KdlValue};

use crate::values::{push_value_node, value_to_f32};

const DEFAULT_WINDOW_OPACITY: f32 = 0.94;
const MIN_WINDOW_OPACITY: f32 = 0.1;
const MAX_WINDOW_OPACITY: f32 = 1.0;
/// Whether a transparent window turns opaque in fullscreen. Wayland
/// compositors must hide what is behind a translucent fullscreen window
/// (xdg-shell; niri paints black there) and macOS gives it a Space of its
/// own, so the panes would only show black; Windows still shows the desktop.
const DEFAULT_FULLSCREEN_OPAQUE: bool = !cfg!(target_os = "windows");
const DEFAULT_QUICK_ASK_TRANSPARENCY: f32 = 23.0 / 255.0;
const DEFAULT_QUICK_ASK_TEXT_SIZE: f32 = 13.0;
pub const DEFAULT_PAGE_IMAGE_LIMIT: usize = 8;
const MIN_QUICK_ASK_TRANSPARENCY: f32 = 0.0;
const MAX_QUICK_ASK_TRANSPARENCY: f32 = 1.0;
const MIN_QUICK_ASK_TEXT_SIZE: f32 = 8.0;
const MAX_QUICK_ASK_TEXT_SIZE: f32 = 32.0;
const DEFAULT_PINNED_TRANSLATION_WIDTH: f32 = 360.0;
const MIN_PINNED_TRANSLATION_WIDTH: f32 = 240.0;
const MAX_PINNED_TRANSLATION_WIDTH: f32 = 640.0;
pub const DEFAULT_PADDLE_OCR_ENDPOINT: &str = "https://paddleocr.aistudio-app.com";
pub const DEFAULT_PADDLE_OCR_MODEL: &str = "PaddleOCR-VL-1.6";
pub const DEFAULT_UNLIMITED_OCR_ENDPOINT: &str = "https://aip.baidubce.com";
pub const DEFAULT_OCR_ENDPOINT: &str = DEFAULT_PADDLE_OCR_ENDPOINT;
pub const DEFAULT_OCR_MODEL: &str = DEFAULT_PADDLE_OCR_MODEL;

mod ai;
mod model;
mod ocr;
mod persistence;
mod provider_secrets;
mod translation;
mod user;
mod window;

pub use ai::{
    AcpAgentConfig, AiApiStyle, AiCapabilityOverride, AiCapabilitySupport, AiModel,
    AiModelCapabilities, AiProvider, AiProviderConfig, AiSettings, AiThinkingLevel,
    AnthropicThinking, ModelLimits, ModelRef, PageImageQuality, PageTextFormat, QuickAskPosition,
    QuickAskSettings, ResolvedAiConfig,
};
pub use model::Settings;
pub use ocr::{
    DEFAULT_LOCAL_OCR_MODEL, LocalOcrSettings, OcrDevice, OcrMode, OcrProvider, OcrSettings,
    PaddleOcrSettings, UnlimitedOcrSettings,
};
pub use persistence::{parse_settings, settings_file};
pub use provider_secrets::{ProviderSecretId, ProviderSecretPersistence, ProviderSecretStatus};
pub use translation::{
    DocumentTranslationMode, PdfContentReviewMode, PdfContentReviewSettings,
    PinnedTranslationSettings, TranslationBoxCorner, TranslationProvider,
    TranslationProviderConfig, TranslationSettings, TranslationTargetLanguage,
};
pub use user::{
    CredentialStore, MemoryCredentials, SystemKeychain, config_owned_secret_ids,
    keychain_secret_ids, load_user_settings, read_keychain_secrets, save_user_settings,
    save_user_settings_to,
};
pub use window::{WindowBackgroundMode, WindowSettings};
