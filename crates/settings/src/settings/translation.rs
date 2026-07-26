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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationProviderConfig {
    pub enabled: bool,
    pub api_key: String,
    pub base_url: Option<String>,
    pub region: Option<String>,
}

impl TranslationProviderConfig {
    pub fn default_for(provider: TranslationProvider) -> Self {
        Self {
            enabled: provider == TranslationProvider::Ai,
            api_key: String::new(),
            base_url: None,
            region: None,
        }
    }

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
        let mut config = Self::default_for(provider);
        if let Some(value) = document.get_arg("enabled").and_then(KdlValue::as_bool) {
            config.enabled = value;
        }
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
        push_value_node(document, "enabled", self.enabled);
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

    pub(super) fn is_default_for(&self, provider: TranslationProvider) -> bool {
        self == &Self::default_for(provider)
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
    pub target_language: String,
    pub pinned: PinnedTranslationSettings,
}

impl Default for TranslationSettings {
    fn default() -> Self {
        Self {
            providers: BTreeMap::from([(
                TranslationProvider::Ai,
                TranslationProviderConfig::default_for(TranslationProvider::Ai),
            )]),
            active_provider: TranslationProvider::Ai,
            source_language: None,
            target_language: DEFAULT_TRANSLATION_TARGET_LANGUAGE.to_string(),
            pinned: PinnedTranslationSettings::default(),
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
        self.target_language = self.target_language.trim().to_string();
        if self.target_language.is_empty() {
            self.target_language = DEFAULT_TRANSLATION_TARGET_LANGUAGE.to_string();
        }
        self.target_language.make_ascii_lowercase();
        self.pinned.sanitize();

        for (provider, config) in &mut self.providers {
            config.sanitize(*provider);
        }

        if !self.provider_config(self.active_provider).enabled {
            self.active_provider = TranslationProvider::ALL
                .into_iter()
                .find(|provider| self.provider_config(*provider).enabled)
                .unwrap_or(TranslationProvider::Ai);
        }
    }

    pub fn provider_config(&self, provider: TranslationProvider) -> TranslationProviderConfig {
        self.providers
            .get(&provider)
            .cloned()
            .unwrap_or_else(|| TranslationProviderConfig::default_for(provider))
    }

    pub fn provider_config_mut(
        &mut self,
        provider: TranslationProvider,
    ) -> &mut TranslationProviderConfig {
        self.providers
            .entry(provider)
            .or_insert_with(|| TranslationProviderConfig::default_for(provider))
    }
}
