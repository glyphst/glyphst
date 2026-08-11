use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TranslationProvider {
    #[default]
    Ai,
    DeepL,
    Google,
    Microsoft,
}

impl TranslationProvider {
    pub const ALL: [Self; 4] = [Self::Ai, Self::DeepL, Self::Google, Self::Microsoft];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ai => "ai",
            Self::DeepL => "deepl",
            Self::Google => "google",
            Self::Microsoft => "microsoft",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ai" => Some(Self::Ai),
            "deepl" => Some(Self::DeepL),
            "google" => Some(Self::Google),
            "microsoft" => Some(Self::Microsoft),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ai => "AI model",
            Self::DeepL => "DeepL",
            Self::Google => "Google Cloud Translation",
            Self::Microsoft => "Microsoft Translator",
        }
    }

    pub fn default_base_url(self) -> Option<&'static str> {
        match self {
            Self::Ai => None,
            Self::DeepL => Some("https://api-free.deepl.com"),
            Self::Google => Some("https://translation.googleapis.com"),
            Self::Microsoft => Some("https://api.cognitive.microsofttranslator.com"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranslationProviderConfig {
    pub api_key: String,
    pub base_url: Option<String>,
    pub region: Option<String>,
}

impl TranslationProviderConfig {
    pub(super) fn sanitize(&mut self, provider: TranslationProvider) {
        self.api_key = self.api_key.trim().to_string();
        self.base_url = self
            .base_url
            .take()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
            .filter(|value| Some(value.as_str()) != provider.default_base_url());
        self.region = self
            .region
            .take()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
    }

    pub(super) fn from_document(provider: TranslationProvider, document: &KdlDocument) -> Self {
        let mut config = Self::default();
        if let Some(value) = document.get_arg("api_key").and_then(KdlValue::as_string) {
            config.api_key = value.to_string();
        }
        config.base_url = document
            .get_arg("base_url")
            .and_then(KdlValue::as_string)
            .map(str::to_string);
        config.region = document
            .get_arg("region")
            .and_then(KdlValue::as_string)
            .map(str::to_string);
        config.sanitize(provider);
        config
    }

    pub(super) fn write_to_document(&self, document: &mut KdlDocument) {
        if !self.api_key.is_empty() {
            push_value_node(document, "api_key", self.api_key.clone());
        }
        if let Some(base_url) = &self.base_url {
            push_value_node(document, "base_url", base_url.clone());
        }
        if let Some(region) = &self.region {
            push_value_node(document, "region", region.clone());
        }
    }

    pub(super) fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TranslationBoxCorner {
    TopLeft,
    #[default]
    TopRight,
    BottomLeft,
    BottomRight,
}

impl TranslationBoxCorner {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TopLeft => "top-left",
            Self::TopRight => "top-right",
            Self::BottomLeft => "bottom-left",
            Self::BottomRight => "bottom-right",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "top-left" => Some(Self::TopLeft),
            "top-right" => Some(Self::TopRight),
            "bottom-left" => Some(Self::BottomLeft),
            "bottom-right" => Some(Self::BottomRight),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DocumentTranslationMode {
    #[default]
    Auto,
    Vertical,
    Horizontal,
    Background,
}

impl DocumentTranslationMode {
    pub const ALL: [Self; 4] = [
        Self::Auto,
        Self::Vertical,
        Self::Horizontal,
        Self::Background,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Vertical => "vertical",
            Self::Horizontal => "horizontal",
            Self::Background => "background",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "vertical" => Some(Self::Vertical),
            "horizontal" => Some(Self::Horizontal),
            "background" => Some(Self::Background),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto split",
            Self::Vertical => "Vertical split (side by side)",
            Self::Horizontal => "Horizontal split (top and bottom)",
            Self::Background => "Run in background",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PdfContentReviewMode {
    #[default]
    Off,
    Ambiguous,
}

impl PdfContentReviewMode {
    pub const ALL: [Self; 2] = [Self::Off, Self::Ambiguous];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Ambiguous => "ambiguous",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "ambiguous" => Some(Self::Ambiguous),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Ambiguous => "Review ambiguous blocks",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfContentReviewSettings {
    pub mode: PdfContentReviewMode,
    pub model: Option<ModelRef>,
}

impl Default for PdfContentReviewSettings {
    fn default() -> Self {
        Self {
            mode: PdfContentReviewMode::Off,
            model: None,
        }
    }
}

impl PdfContentReviewSettings {
    fn sanitize(&mut self) {
        if let Some(model) = self.model.as_mut() {
            model.sanitize();
            if model.id.is_empty() {
                self.model = None;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PinnedTranslationSettings {
    pub width: f32,
    pub corner: TranslationBoxCorner,
}

impl Default for PinnedTranslationSettings {
    fn default() -> Self {
        Self {
            width: DEFAULT_PINNED_TRANSLATION_WIDTH,
            corner: TranslationBoxCorner::TopRight,
        }
    }
}

impl PinnedTranslationSettings {
    pub(super) fn sanitize(&mut self) {
        self.width = if self.width.is_finite() {
            self.width
                .clamp(MIN_PINNED_TRANSLATION_WIDTH, MAX_PINNED_TRANSLATION_WIDTH)
        } else {
            DEFAULT_PINNED_TRANSLATION_WIDTH
        };
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TranslationSettings {
    pub providers: BTreeMap<TranslationProvider, TranslationProviderConfig>,
    pub active_provider: TranslationProvider,
    pub source_language: Option<String>,
    pub target_language: TranslationTargetLanguage,
    pub document_mode: DocumentTranslationMode,
    pub pinned: PinnedTranslationSettings,
    pub content_review: PdfContentReviewSettings,
}

impl Default for TranslationSettings {
    fn default() -> Self {
        Self {
            providers: BTreeMap::new(),
            active_provider: TranslationProvider::Ai,
            source_language: None,
            target_language: TranslationTargetLanguage::UiLanguage,
            document_mode: DocumentTranslationMode::Auto,
            pinned: PinnedTranslationSettings::default(),
            content_review: PdfContentReviewSettings::default(),
        }
    }
}

impl TranslationSettings {
    pub(super) fn sanitize(&mut self) {
        self.source_language = self
            .source_language
            .take()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("auto"));
        self.target_language.sanitize();
        self.pinned.sanitize();
        self.content_review.sanitize();

        for (provider, config) in &mut self.providers {
            config.sanitize(*provider);
        }
    }

    pub fn provider_config(&self, provider: TranslationProvider) -> TranslationProviderConfig {
        self.providers.get(&provider).cloned().unwrap_or_default()
    }

    /// Resolve the configured target at request time so inherited targets follow
    /// changes to the application's language without rewriting the setting.
    pub fn resolved_target_language(&self) -> String {
        match &self.target_language {
            TranslationTargetLanguage::UiLanguage => {
                crate::localize::ui_translation_language_code().to_string()
            }
            TranslationTargetLanguage::Explicit(language) => language.clone(),
        }
    }

    pub fn provider_config_mut(
        &mut self,
        provider: TranslationProvider,
    ) -> &mut TranslationProviderConfig {
        self.providers.entry(provider).or_default()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TranslationTargetLanguage {
    #[default]
    UiLanguage,
    Explicit(String),
}

impl TranslationTargetLanguage {
    pub const UI_LANGUAGE_VALUE: &'static str = "ui";

    pub fn parse(value: &str) -> Self {
        let value = value.trim();
        if value.is_empty() || value.eq_ignore_ascii_case(Self::UI_LANGUAGE_VALUE) {
            Self::UiLanguage
        } else {
            Self::Explicit(value.to_ascii_lowercase())
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::UiLanguage => Self::UI_LANGUAGE_VALUE,
            Self::Explicit(language) => language,
        }
    }

    fn sanitize(&mut self) {
        *self = Self::parse(self.as_str());
    }
}
