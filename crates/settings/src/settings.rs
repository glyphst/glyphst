use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use glyphst_core::{EpubReaderPreferences, EpubTheme};
use gpui::{App, Font, FontFallbacks, FontWeight, Global, Window, WindowBackgroundAppearance};
use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};

use crate::theme::{
    DEFAULT_DARK_THEME_ID, DEFAULT_LIGHT_THEME_ID, PartialTheme, Theme, ThemeAppearance,
    ThemeCatalog, ThemeSelection, apply_component_overrides, push_value_node, value_to_f32,
};

pub const DEFAULT_ZOOM: f32 = 1.0;
pub const DEFAULT_ZOOM_STEP: f32 = 1.1;
pub const DEFAULT_SMOOTH_SCROLLING: bool = true;
pub const DEFAULT_UI_FONT_FAMILY: &str = ".ZedSans";
pub const DEFAULT_UI_FONT_SIZE: f32 = 13.0;
pub const DEFAULT_UI_FONT_WEIGHT: f32 = 400.0;

const MIN_ZOOM_SETTING: f32 = 0.25;
const MAX_ZOOM_SETTING: f32 = 8_192.0;
const MIN_ZOOM_STEP: f32 = 1.01;
const MAX_ZOOM_STEP: f32 = 4.0;
const MIN_UI_FONT_SIZE: f32 = 6.0;
const MAX_UI_FONT_SIZE: f32 = 100.0;
const MIN_UI_FONT_WEIGHT: f32 = 100.0;
const MAX_UI_FONT_WEIGHT: f32 = 900.0;
const DEFAULT_WINDOW_OPACITY: f32 = 0.94;
const MIN_WINDOW_OPACITY: f32 = 0.1;
const MAX_WINDOW_OPACITY: f32 = 1.0;
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
const DEFAULT_UI_FONT_FALLBACKS: &[&str] = &[
    "Noto Sans CJK SC",
    "Noto Sans SC",
    "Source Han Sans SC",
    "WenQuanYi Micro Hei",
];

mod ai;
mod general;
mod model;
mod ocr;
mod persistence;
mod provider_secrets;
mod store;
mod translation;

pub use ai::{
    AiApiStyle, AiCapabilityOverride, AiCapabilitySupport, AiModel, AiModelCapabilities,
    AiProvider, AiProviderConfig, AiSettings, AiThinkingLevel, ModelRef, PageImageQuality,
    PageTextFormat, QuickAskPosition, QuickAskSettings, ResolvedAiConfig,
};
pub use general::{
    AppLanguage, BehaviorSettings, CopyFormat, DocumentProgressDisplay, DocumentThemeSettings,
    DocumentViewSettings, FontSettings, KeyBindingOverride, KeymapPreset, NewTabSettings,
    NewTabViewMode, PathChooserBackend, RestoreOnStartup, TabBarVisibility, WindowBackgroundMode,
    WindowSettings, ui_rems,
};
pub use model::Settings;
pub use ocr::{OcrMode, OcrProvider, OcrSettings, PaddleOcrSettings, UnlimitedOcrSettings};
pub use persistence::{parse_settings, user_themes_dir};
pub use provider_secrets::{ProviderSecretId, ProviderSecretPersistence, ProviderSecretStatus};
pub use store::{SettingsStore, setup_ui_font, sync_component_theme};
pub use translation::{
    DocumentTranslationMode, PdfContentReviewMode, PdfContentReviewSettings,
    PinnedTranslationSettings, TranslationBoxCorner, TranslationProvider,
    TranslationProviderConfig, TranslationSettings, TranslationTargetLanguage,
};

#[cfg(test)]
mod tests;
