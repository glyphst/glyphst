use std::{
    collections::{BTreeMap, HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use glyphst_settings::{
    AiApiStyle, ResolvedAiConfig, Settings, TranslationProvider, TranslationProviderConfig,
};
use reqwest::{StatusCode, blocking::Client};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_BATCH_TEXTS: usize = 64;
const MAX_BATCH_CHARS: usize = 24_000;
const SEMANTIC_PROTOCOL_VERSION: &str = "semantic-v5-json-items-adjacency-styles-structured-lines";
const GOOGLE_SEMANTIC_PROTOCOL_VERSION: &str = "google-semantic-v4-html-spans-inline-anchors-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetLanguage {
    pub code: &'static str,
    pub label: &'static str,
}

pub const TARGET_LANGUAGES: &[TargetLanguage] = &[
    TargetLanguage {
        code: "ar",
        label: "Arabic",
    },
    TargetLanguage {
        code: "bg",
        label: "Bulgarian",
    },
    TargetLanguage {
        code: "cs",
        label: "Czech",
    },
    TargetLanguage {
        code: "da",
        label: "Danish",
    },
    TargetLanguage {
        code: "de",
        label: "German",
    },
    TargetLanguage {
        code: "el",
        label: "Greek",
    },
    TargetLanguage {
        code: "en",
        label: "English",
    },
    TargetLanguage {
        code: "es",
        label: "Spanish",
    },
    TargetLanguage {
        code: "et",
        label: "Estonian",
    },
    TargetLanguage {
        code: "fi",
        label: "Finnish",
    },
    TargetLanguage {
        code: "fr",
        label: "French",
    },
    TargetLanguage {
        code: "he",
        label: "Hebrew",
    },
    TargetLanguage {
        code: "hi",
        label: "Hindi",
    },
    TargetLanguage {
        code: "hu",
        label: "Hungarian",
    },
    TargetLanguage {
        code: "id",
        label: "Indonesian",
    },
    TargetLanguage {
        code: "it",
        label: "Italian",
    },
    TargetLanguage {
        code: "ja",
        label: "Japanese",
    },
    TargetLanguage {
        code: "ko",
        label: "Korean",
    },
    TargetLanguage {
        code: "lt",
        label: "Lithuanian",
    },
    TargetLanguage {
        code: "lv",
        label: "Latvian",
    },
    TargetLanguage {
        code: "nl",
        label: "Dutch",
    },
    TargetLanguage {
        code: "no",
        label: "Norwegian",
    },
    TargetLanguage {
        code: "pl",
        label: "Polish",
    },
    TargetLanguage {
        code: "pt",
        label: "Portuguese",
    },
    TargetLanguage {
        code: "pt-br",
        label: "Portuguese (Brazil)",
    },
    TargetLanguage {
        code: "pt-pt",
        label: "Portuguese (Portugal)",
    },
    TargetLanguage {
        code: "ro",
        label: "Romanian",
    },
    TargetLanguage {
        code: "ru",
        label: "Russian",
    },
    TargetLanguage {
        code: "sk",
        label: "Slovak",
    },
    TargetLanguage {
        code: "sl",
        label: "Slovenian",
    },
    TargetLanguage {
        code: "sv",
        label: "Swedish",
    },
    TargetLanguage {
        code: "th",
        label: "Thai",
    },
    TargetLanguage {
        code: "tr",
        label: "Turkish",
    },
    TargetLanguage {
        code: "uk",
        label: "Ukrainian",
    },
    TargetLanguage {
        code: "vi",
        label: "Vietnamese",
    },
    TargetLanguage {
        code: "zh-hans",
        label: "Chinese (Simplified)",
    },
    TargetLanguage {
        code: "zh-hant",
        label: "Chinese (Traditional)",
    },
];

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LanguageCode(String);

impl LanguageCode {
    pub fn new(value: impl AsRef<str>) -> Result<Self> {
        let value = value.as_ref().trim().to_ascii_lowercase();
        if value.is_empty() || value.len() > 35 {
            return Err(anyhow!("language code must be between 1 and 35 characters"));
        }
        if !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
        {
            return Err(anyhow!("language code contains unsupported characters"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationRequest {
    pub provider: TranslationProvider,
    pub source_language: Option<LanguageCode>,
    pub target_language: LanguageCode,
    pub texts: Vec<String>,
    pub context_before: Option<String>,
    pub context_after: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationResponse {
    pub translations: Vec<String>,
    pub detected_source_language: Option<LanguageCode>,
}

/// Immutable provider configuration captured when a translation is submitted.
///
/// Keeping this snapshot separate from [`Settings`] prevents background work from
/// depending on unrelated application preferences or observing later mutations.
#[derive(Clone, Debug)]
pub struct TranslationExecutionConfig {
    provider: TranslationProvider,
    providers: BTreeMap<TranslationProvider, TranslationProviderConfig>,
    ai: ResolvedAiConfig,
}

impl TranslationExecutionConfig {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            provider: settings.translation.active_provider,
            providers: settings.translation.providers.clone(),
            ai: settings.ai.resolved_translation(),
        }
    }

    pub fn provider(&self) -> TranslationProvider {
        self.provider
    }

    pub fn provider_config(&self, provider: TranslationProvider) -> TranslationProviderConfig {
        self.providers.get(&provider).cloned().unwrap_or_default()
    }

    pub fn provider_identity(&self, provider: TranslationProvider) -> String {
        semantic_provider_identity(self, provider)
    }
}

impl From<&Settings> for TranslationExecutionConfig {
    fn from(settings: &Settings) -> Self {
        Self::from_settings(settings)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SemanticTranslationUnit {
    pub id: String,
    pub role: String,
    pub text: String,
    pub protected_tokens: Vec<SemanticProtectedToken>,
    pub context_before: Option<String>,
    pub context_after: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SemanticProtectedToken {
    pub placeholder: String,
    pub original: String,
    pub kind: SemanticProtectedTokenKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SemanticProtectedTokenKind {
    Formula,
    Citation,
    Url,
    NumericLiteral,
    Identifier,
    StructuralMarker,
    InlineObject,
    LineBreak,
    StyleBoundary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticTranslation {
    pub id: String,
    pub text: String,
    pub cached: bool,
    pub validation_error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub supports_batches: bool,
    pub supports_auto_detect: bool,
    pub supports_dictionary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderMetadata {
    pub provider: TranslationProvider,
    pub label: &'static str,
    pub capabilities: ProviderCapabilities,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationError {
    pub provider: TranslationProvider,
    pub category: TranslationErrorCategory,
    pub message: String,
    pub status: Option<u16>,
    pub retryable: bool,
    pub retry_after: Option<Duration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranslationErrorCategory {
    Authentication,
    RateLimited,
    ServiceUnavailable,
    Timeout,
    InvalidRequest,
    InvalidResponse,
    Network,
    Cache,
}

impl std::fmt::Display for TranslationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(formatter)
    }
}

impl std::error::Error for TranslationError {}

pub trait DictionaryProvider {
    fn lookup(&self, _word: &str, _source: &LanguageCode, _target: &LanguageCode) -> Result<()> {
        Err(anyhow!("dictionary lookup is not available yet"))
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedTranslation {
    translations: Vec<String>,
    detected_source_language: Option<String>,
    updated_at: u64,
}

#[derive(Clone)]
pub struct TranslationService {
    client: Client,
    cache: Arc<Mutex<TranslationCache>>,
}

impl TranslationService {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("failed to create translation HTTP client")?;
        Ok(Self {
            client,
            cache: Arc::new(Mutex::new(TranslationCache::memory())),
        })
    }

    pub fn initialize_persistent_cache(&self) -> Result<()> {
        let mut persistent_cache = TranslationCache::open()?;
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| anyhow!("translation cache is unavailable"))?;
        persistent_cache.insert_many(cache.memory.clone())?;
        *cache = persistent_cache;
        Ok(())
    }

    pub fn clear_cache(&self) -> Result<()> {
        self.cache
            .lock()
            .map_err(|_| anyhow!("translation cache is unavailable"))?
            .clear()
    }

    pub fn provider_metadata(provider: TranslationProvider) -> ProviderMetadata {
        ProviderMetadata {
            provider,
            label: provider.label(),
            capabilities: ProviderCapabilities {
                supports_batches: true,
                supports_auto_detect: true,
                supports_dictionary: false,
            },
        }
    }

    pub fn translate(
        &self,
        config: &TranslationExecutionConfig,
        request: TranslationRequest,
    ) -> std::result::Result<TranslationResponse, TranslationError> {
        let request = sanitize_request(request).map_err(|error| TranslationError {
            provider: config.provider(),
            category: TranslationErrorCategory::InvalidRequest,
            message: error.to_string(),
            status: None,
            retryable: false,
            retry_after: None,
        })?;
        let cache_key = cache_key(&request);
        if let Some(cached) = self
            .cache
            .lock()
            .map_err(|_| TranslationError {
                provider: request.provider,
                category: TranslationErrorCategory::Cache,
                message: "translation cache is unavailable".to_string(),
                status: None,
                retryable: true,
                retry_after: None,
            })?
            .get(&cache_key)
        {
            return Ok(TranslationResponse {
                translations: cached.translations,
                detected_source_language: cached
                    .detected_source_language
                    .and_then(|language| LanguageCode::new(language).ok()),
            });
        }

        let response = self.request_provider(config, &request)?;
        let cached = CachedTranslation {
            translations: response.translations.clone(),
            detected_source_language: response
                .detected_source_language
                .as_ref()
                .map(|language| language.as_str().to_string()),
            updated_at: unix_seconds(),
        };
        if let Ok(mut cache) = self.cache.lock()
            && let Err(error) = cache.insert(&cache_key, cached)
        {
            eprintln!("failed to cache translation: {error:#}");
        }
        Ok(response)
    }

    pub fn translate_batches(
        &self,
        config: &TranslationExecutionConfig,
        provider: TranslationProvider,
        source_language: Option<LanguageCode>,
        target_language: LanguageCode,
        texts: Vec<String>,
    ) -> std::result::Result<Vec<String>, TranslationError> {
        let mut translated = Vec::with_capacity(texts.len());
        for texts in split_batches(texts) {
            let batch = texts.clone();
            let response = self.translate(
                config,
                TranslationRequest {
                    provider,
                    source_language: source_language.clone(),
                    target_language: target_language.clone(),
                    texts: batch,
                    context_before: None,
                    context_after: None,
                },
            );
            match response {
                Ok(response) => translated.extend(response.translations),
                Err(error) if error.retryable && texts.len() > 1 => {
                    for text in texts {
                        let response = self.translate(
                            config,
                            TranslationRequest {
                                provider,
                                source_language: source_language.clone(),
                                target_language: target_language.clone(),
                                texts: vec![text],
                                context_before: None,
                                context_after: None,
                            },
                        )?;
                        translated.extend(response.translations);
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(translated)
    }

    pub fn translate_semantic_batch(
        &self,
        config: &TranslationExecutionConfig,
        provider: TranslationProvider,
        source_language: Option<LanguageCode>,
        target_language: LanguageCode,
        units: Vec<SemanticTranslationUnit>,
    ) -> std::result::Result<Vec<SemanticTranslation>, TranslationError> {
        if units.is_empty() {
            return Ok(Vec::new());
        }
        if units.len() > MAX_BATCH_TEXTS
            || units.iter().map(|unit| unit.text.len()).sum::<usize>() > MAX_BATCH_CHARS
        {
            return Err(TranslationError {
                provider,
                category: TranslationErrorCategory::InvalidRequest,
                message: "semantic translation request exceeds the provider batch limit"
                    .to_string(),
                status: None,
                retryable: false,
                retry_after: None,
            });
        }
        let mut translations = Vec::with_capacity(units.len());
        let mut missing = Vec::new();
        {
            let mut cache = self.cache.lock().map_err(|_| TranslationError {
                provider,
                category: TranslationErrorCategory::Cache,
                message: "translation cache is unavailable".to_string(),
                status: None,
                retryable: true,
                retry_after: None,
            })?;
            for unit in &units {
                let key = semantic_cache_key(
                    config,
                    provider,
                    source_language.as_ref(),
                    &target_language,
                    unit,
                );
                match cache.get(&key).and_then(|cached| {
                    (cached.translations.len() == 1
                        && validate_semantic_cache_entry(unit, &cached.translations[0]))
                    .then(|| SemanticTranslation {
                        id: unit.id.clone(),
                        text: cached.translations[0].clone(),
                        cached: true,
                        validation_error: None,
                    })
                }) {
                    Some(translation) => translations.push(translation),
                    None => missing.push(unit.clone()),
                }
            }
        }
        if missing.is_empty() {
            return order_semantic_translations(&units, translations, provider);
        }

        let requested = match provider {
            TranslationProvider::Ai => self.translate_semantic_with_ai(
                config,
                source_language.as_ref(),
                &target_language,
                &missing,
            )?,
            TranslationProvider::Google => self.translate_semantic_with_google(
                config,
                source_language.as_ref(),
                &target_language,
                &missing,
            )?,
            TranslationProvider::DeepL | TranslationProvider::Microsoft => {
                let request = TranslationRequest {
                    provider,
                    source_language: source_language.clone(),
                    target_language: target_language.clone(),
                    texts: missing.iter().map(|unit| unit.text.clone()).collect(),
                    context_before: missing.first().and_then(|unit| unit.context_before.clone()),
                    context_after: missing.last().and_then(|unit| unit.context_after.clone()),
                };
                let response = self.request_provider(config, &request)?;
                missing
                    .iter()
                    .zip(response.translations)
                    .map(|(unit, text)| SemanticTranslation {
                        id: unit.id.clone(),
                        text,
                        cached: false,
                        validation_error: None,
                    })
                    .collect()
            }
        };
        translations.extend(requested);
        order_semantic_translations(&units, translations, provider)
    }

    pub fn cache_validated_semantic_translation(
        &self,
        config: &TranslationExecutionConfig,
        provider: TranslationProvider,
        source_language: Option<&LanguageCode>,
        target_language: &LanguageCode,
        unit: &SemanticTranslationUnit,
        translated_text: &str,
    ) {
        self.cache_validated_semantic_translations(
            config,
            provider,
            source_language,
            target_language,
            &[(unit.clone(), translated_text.to_string())],
        );
    }

    pub fn cache_validated_semantic_translations(
        &self,
        config: &TranslationExecutionConfig,
        provider: TranslationProvider,
        source_language: Option<&LanguageCode>,
        target_language: &LanguageCode,
        translations: &[(SemanticTranslationUnit, String)],
    ) {
        let updated_at = unix_seconds();
        let detected_source_language =
            source_language.map(|language| language.as_str().to_string());
        let entries = translations
            .iter()
            .filter(|(unit, translated)| validate_semantic_cache_entry(unit, translated))
            .map(|(unit, translated)| {
                (
                    semantic_cache_key(config, provider, source_language, target_language, unit),
                    CachedTranslation {
                        translations: vec![translated.clone()],
                        detected_source_language: detected_source_language.clone(),
                        updated_at,
                    },
                )
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return;
        }
        if let Ok(mut cache) = self.cache.lock()
            && let Err(error) = cache.insert_many(entries)
        {
            eprintln!("failed to cache semantic translations: {error:#}");
        }
    }

    pub fn translate_semantic_single_recovery(
        &self,
        config: &TranslationExecutionConfig,
        source_language: Option<&LanguageCode>,
        target_language: &LanguageCode,
        unit: SemanticTranslationUnit,
    ) -> std::result::Result<SemanticTranslation, TranslationError> {
        let resolved = config.ai.clone();
        let input = glyphst_ai::translation::SemanticTranslationInput {
            id: unit.id,
            role: unit.role,
            text: unit.text,
            context_before: None,
            context_after: None,
        };
        glyphst_ai::translation::translate_semantic_single_with_client(
            &self.client,
            resolved,
            &input,
            source_language.map(LanguageCode::as_str),
            target_language.as_str(),
        )
        .map(|output| SemanticTranslation {
            id: output.id,
            text: output.text,
            cached: false,
            validation_error: None,
        })
        .map_err(ai_translation_error)
    }

    fn request_provider(
        &self,
        config: &TranslationExecutionConfig,
        request: &TranslationRequest,
    ) -> std::result::Result<TranslationResponse, TranslationError> {
        match request.provider {
            TranslationProvider::Ai => self.translate_with_ai(config, request),
            TranslationProvider::DeepL => self.translate_with_deepl(config, request),
            TranslationProvider::Google => self.translate_with_google(config, request),
            TranslationProvider::Microsoft => self.translate_with_microsoft(config, request),
        }
    }

    fn translate_semantic_with_ai(
        &self,
        config: &TranslationExecutionConfig,
        source_language: Option<&LanguageCode>,
        target_language: &LanguageCode,
        units: &[SemanticTranslationUnit],
    ) -> std::result::Result<Vec<SemanticTranslation>, TranslationError> {
        let resolved = config.ai.clone();
        if !resolved.can_request() {
            return Err(TranslationError {
                provider: TranslationProvider::Ai,
                category: TranslationErrorCategory::Authentication,
                message: resolved.missing_credentials_message(),
                status: None,
                retryable: false,
                retry_after: None,
            });
        }
        let inputs = units
            .iter()
            .map(|unit| glyphst_ai::translation::SemanticTranslationInput {
                id: unit.id.clone(),
                role: unit.role.clone(),
                text: unit.text.clone(),
                context_before: unit.context_before.clone(),
                context_after: unit.context_after.clone(),
            })
            .collect::<Vec<_>>();
        let outputs = glyphst_ai::translation::translate_semantic_batch_with_client(
            &self.client,
            resolved,
            &inputs,
            source_language.map(LanguageCode::as_str),
            target_language.as_str(),
        )
        .map_err(ai_translation_error)?;
        let mut by_id = outputs
            .into_iter()
            .map(|output| (output.id, output.text))
            .collect::<HashMap<_, _>>();
        Ok(units
            .iter()
            .filter_map(|unit| {
                by_id.remove(&unit.id).map(|text| SemanticTranslation {
                    id: unit.id.clone(),
                    text,
                    cached: false,
                    validation_error: None,
                })
            })
            .collect())
    }

    fn translate_semantic_with_google(
        &self,
        config: &TranslationExecutionConfig,
        source_language: Option<&LanguageCode>,
        target_language: &LanguageCode,
        units: &[SemanticTranslationUnit],
    ) -> std::result::Result<Vec<SemanticTranslation>, TranslationError> {
        let (inline_anchored, ordinary): (Vec<_>, Vec<_>) =
            units.iter().cloned().partition(|unit| {
                unit.protected_tokens
                    .iter()
                    .any(|token| token.kind == SemanticProtectedTokenKind::InlineObject)
            });
        let mut translations = self.translate_google_semantic_units(
            config,
            source_language,
            target_language,
            &ordinary,
        )?;
        for unit in &inline_anchored {
            translations.push(self.translate_google_inline_anchored_unit(
                config,
                source_language,
                target_language,
                unit,
            )?);
        }
        order_semantic_translations(units, translations, TranslationProvider::Google)
    }

    fn translate_google_semantic_units(
        &self,
        config: &TranslationExecutionConfig,
        source_language: Option<&LanguageCode>,
        target_language: &LanguageCode,
        units: &[SemanticTranslationUnit],
    ) -> std::result::Result<Vec<SemanticTranslation>, TranslationError> {
        if units.is_empty() {
            return Ok(Vec::new());
        }
        let prepared = units
            .iter()
            .map(prepare_google_semantic_unit)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|message| TranslationError {
                provider: TranslationProvider::Google,
                category: TranslationErrorCategory::InvalidRequest,
                message,
                status: None,
                retryable: false,
                retry_after: None,
            })?;
        let batch = self.request_google(
            config,
            source_language,
            target_language,
            prepared
                .iter()
                .map(|prepared| prepared.html.clone())
                .collect(),
            "html",
        )?;
        let mut translations = Vec::with_capacity(units.len());
        let mut invalid = Vec::new();
        for (index, ((unit, prepared), translated)) in units
            .iter()
            .zip(&prepared)
            .zip(batch.translations)
            .enumerate()
        {
            match restore_google_semantic_html(prepared, &translated, "batch HTML").and_then(
                |text| {
                    validate_google_semantic_output(
                        unit,
                        prepared,
                        &text,
                        source_language,
                        target_language,
                        "batch HTML",
                    )
                    .map(|()| text)
                },
            ) {
                Ok(text) => translations.push(SemanticTranslation {
                    id: unit.id.clone(),
                    text,
                    cached: false,
                    validation_error: None,
                }),
                Err(error) => invalid.push((index, translated, error)),
            }
        }

        for (index, batch_text, batch_error) in invalid {
            let unit = &units[index];
            let prepared = &prepared[index];
            let singleton = self.request_google(
                config,
                source_language,
                target_language,
                vec![prepared.html.clone()],
                "html",
            );
            let (last_text, singleton_error) = match singleton {
                Ok(response) => {
                    let singleton_text =
                        response.translations.into_iter().next().unwrap_or_default();
                    match restore_google_semantic_html(prepared, &singleton_text, "singleton HTML")
                        .and_then(|text| {
                            validate_google_semantic_output(
                                unit,
                                prepared,
                                &text,
                                source_language,
                                target_language,
                                "singleton HTML",
                            )
                            .map(|()| text)
                        }) {
                        Ok(text) => {
                            translations.push(SemanticTranslation {
                                id: unit.id.clone(),
                                text,
                                cached: false,
                                validation_error: None,
                            });
                            continue;
                        }
                        Err(error) => (singleton_text, error),
                    }
                }
                Err(error) if error.retryable => return Err(error),
                Err(error) => (
                    batch_text,
                    format!("singleton HTML request failed: {}", error.message),
                ),
            };

            match self.translate_google_semantic_segments(
                config,
                source_language,
                target_language,
                prepared,
            ) {
                Ok(text)
                    if validate_google_semantic_output(
                        unit,
                        prepared,
                        &text,
                        source_language,
                        target_language,
                        "segmented HTML",
                    )
                    .is_ok() =>
                {
                    translations.push(SemanticTranslation {
                        id: unit.id.clone(),
                        text,
                        cached: false,
                        validation_error: None,
                    });
                }
                Ok(text) => {
                    let segmented_error = validate_google_semantic_output(
                        unit,
                        prepared,
                        &text,
                        source_language,
                        target_language,
                        "segmented HTML",
                    )
                    .unwrap_err();
                    translations.push(SemanticTranslation {
                        id: unit.id.clone(),
                        text,
                        cached: false,
                        validation_error: Some(format!(
                            "Google semantic recovery failed after segmented HTML; {batch_error}; \
                             {singleton_error}; {segmented_error}"
                        )),
                    });
                }
                Err(error) if error.retryable => return Err(error),
                Err(error) => {
                    translations.push(SemanticTranslation {
                        id: unit.id.clone(),
                        text: last_text,
                        cached: false,
                        validation_error: Some(format!(
                            "Google semantic recovery failed after segmented HTML; {batch_error}; \
                             {singleton_error}; segmented HTML request failed: {}",
                            error.message
                        )),
                    });
                }
            }
        }
        order_semantic_translations(units, translations, TranslationProvider::Google)
    }

    fn translate_google_inline_anchored_unit(
        &self,
        config: &TranslationExecutionConfig,
        source_language: Option<&LanguageCode>,
        target_language: &LanguageCode,
        unit: &SemanticTranslationUnit,
    ) -> std::result::Result<SemanticTranslation, TranslationError> {
        let anchored =
            prepare_google_inline_anchored_unit(unit).map_err(|message| TranslationError {
                provider: TranslationProvider::Google,
                category: TranslationErrorCategory::InvalidRequest,
                message,
                status: None,
                retryable: false,
                retry_after: None,
            })?;
        let translatable = anchored
            .chunks
            .iter()
            .filter(|chunk| chunk.translatable)
            .map(|chunk| chunk.unit.clone())
            .collect::<Vec<_>>();
        let mut translated_chunks = Vec::with_capacity(translatable.len());
        let mut batch = Vec::new();
        let mut batch_chars = 0usize;
        for chunk in translatable {
            if !batch.is_empty()
                && (batch.len() == MAX_BATCH_TEXTS
                    || batch_chars + chunk.text.len() > MAX_BATCH_CHARS)
            {
                translated_chunks.extend(self.translate_google_semantic_units(
                    config,
                    source_language,
                    target_language,
                    &batch,
                )?);
                batch.clear();
                batch_chars = 0;
            }
            batch_chars += chunk.text.len();
            batch.push(chunk);
        }
        if !batch.is_empty() {
            translated_chunks.extend(self.translate_google_semantic_units(
                config,
                source_language,
                target_language,
                &batch,
            )?);
        }

        let mut translated_by_id = translated_chunks
            .into_iter()
            .map(|translation| (translation.id.clone(), translation))
            .collect::<HashMap<_, _>>();
        let mut text = String::with_capacity(unit.text.len());
        let mut validation_errors = Vec::new();
        for (index, chunk) in anchored.chunks.iter().enumerate() {
            if chunk.translatable {
                match translated_by_id.remove(&chunk.unit.id) {
                    Some(translation) => {
                        if let Some(error) = translation.validation_error {
                            validation_errors.push(format!(
                                "inline chunk {} failed validation: {error}",
                                index + 1
                            ));
                        }
                        text.push_str(&restore_google_edge_boundaries(
                            &chunk.unit,
                            translation.text.trim_matches(char::is_whitespace),
                            target_language,
                            &chunk.leading_boundary,
                            &chunk.trailing_boundary,
                        ));
                    }
                    None => {
                        validation_errors.push(format!(
                            "inline chunk {} did not receive a translation",
                            index + 1
                        ));
                        text.push_str(&chunk.unit.text);
                    }
                }
            } else {
                text.push_str(&chunk.unit.text);
            }
            if let Some(anchor) = anchored.anchors.get(index) {
                text.push_str(anchor);
            }
        }
        if let Err(error) = validate_google_semantic_output(
            unit,
            &anchored.prepared,
            &text,
            source_language,
            target_language,
            "inline-anchor HTML",
        ) {
            validation_errors.push(error);
        }

        Ok(SemanticTranslation {
            id: unit.id.clone(),
            text,
            cached: false,
            validation_error: (!validation_errors.is_empty()).then(|| {
                format!(
                    "Google inline-anchor translation failed: {}",
                    validation_errors.join("; ")
                )
            }),
        })
    }

    fn translate_google_semantic_segments(
        &self,
        config: &TranslationExecutionConfig,
        source_language: Option<&LanguageCode>,
        target_language: &LanguageCode,
        prepared: &PreparedGoogleSemanticUnit,
    ) -> std::result::Result<String, TranslationError> {
        let translatable = prepared
            .segments
            .iter()
            .filter_map(|segment| match segment {
                GoogleSemanticSegment::NaturalLanguage(text)
                    if text.chars().any(char::is_alphabetic) =>
                {
                    Some(html_escape(text))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        if translatable.is_empty() {
            return Ok(reassemble_google_segments(prepared, &[]));
        }
        let response = self.request_google(
            config,
            source_language,
            target_language,
            translatable,
            "html",
        )?;
        let translated = response
            .translations
            .into_iter()
            .map(|text| html_unescape(&text))
            .collect::<Vec<_>>();
        Ok(reassemble_google_segments(prepared, &translated))
    }

    fn translate_with_ai(
        &self,
        config: &TranslationExecutionConfig,
        request: &TranslationRequest,
    ) -> std::result::Result<TranslationResponse, TranslationError> {
        let resolved = config.ai.clone();
        if !resolved.can_request() {
            return Err(TranslationError {
                provider: TranslationProvider::Ai,
                category: TranslationErrorCategory::Authentication,
                message: resolved.missing_credentials_message(),
                status: None,
                retryable: false,
                retry_after: None,
            });
        }
        let translations = glyphst_ai::translation::translate_batch_with_client(
            &self.client,
            resolved,
            &request.texts,
            request.source_language.as_ref().map(LanguageCode::as_str),
            request.target_language.as_str(),
        )
        .map_err(ai_translation_error)?;
        Ok(TranslationResponse {
            translations,
            detected_source_language: request.source_language.clone(),
        })
    }

    fn translate_with_deepl(
        &self,
        execution: &TranslationExecutionConfig,
        request: &TranslationRequest,
    ) -> std::result::Result<TranslationResponse, TranslationError> {
        let config = provider_config(execution, TranslationProvider::DeepL)?;
        let url = endpoint(&config, TranslationProvider::DeepL, "/v2/translate")?;
        let mut body = json!({
            "text": request.texts,
            "target_lang": deepl_language(request.target_language.as_str()),
        });
        if let Some(source) = request.source_language.as_ref() {
            body["source_lang"] = json!(deepl_language(source.as_str()));
        }
        let response = self
            .client
            .post(url)
            .header(
                "Authorization",
                format!("DeepL-Auth-Key {}", config.api_key),
            )
            .json(&body)
            .send()
            .map_err(|error| request_error(TranslationProvider::DeepL, error))?;
        let value = json_response(TranslationProvider::DeepL, response)?;
        let translations = value
            .get("translations")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed_response(TranslationProvider::DeepL))?
            .iter()
            .map(|value| {
                value
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| malformed_response(TranslationProvider::DeepL))
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        validate_response_count(
            TranslationProvider::DeepL,
            &translations,
            request.texts.len(),
        )?;
        let detected_source_language = value
            .pointer("/translations/0/detected_source_language")
            .and_then(Value::as_str)
            .and_then(|value| LanguageCode::new(value).ok());
        Ok(TranslationResponse {
            translations,
            detected_source_language,
        })
    }

    fn translate_with_google(
        &self,
        config: &TranslationExecutionConfig,
        request: &TranslationRequest,
    ) -> std::result::Result<TranslationResponse, TranslationError> {
        let mut response = self.request_google(
            config,
            request.source_language.as_ref(),
            &request.target_language,
            request.texts.clone(),
            "text",
        )?;
        response.translations = response
            .translations
            .into_iter()
            .map(|translation| html_unescape(&translation))
            .collect();
        Ok(response)
    }

    fn request_google(
        &self,
        execution: &TranslationExecutionConfig,
        source_language: Option<&LanguageCode>,
        target_language: &LanguageCode,
        texts: Vec<String>,
        format: &str,
    ) -> std::result::Result<TranslationResponse, TranslationError> {
        let config = provider_config(execution, TranslationProvider::Google)?;
        let url = endpoint(
            &config,
            TranslationProvider::Google,
            "/language/translate/v2",
        )?;
        let mut body = json!({
            "q": texts,
            "target": google_language(target_language.as_str()),
            "format": format,
        });
        if let Some(source) = source_language {
            body["source"] = json!(google_language(source.as_str()));
        }
        let response = self
            .client
            .post(url)
            .query(&[("key", config.api_key.as_str())])
            .json(&body)
            .send()
            .map_err(|error| request_error(TranslationProvider::Google, error))?;
        let value = json_response(TranslationProvider::Google, response)?;
        let translations = value
            .pointer("/data/translations")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed_response(TranslationProvider::Google))?
            .iter()
            .map(|value| {
                value
                    .get("translatedText")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| malformed_response(TranslationProvider::Google))
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        validate_response_count(TranslationProvider::Google, &translations, texts.len())?;
        let detected_source_language = value
            .pointer("/data/translations/0/detectedSourceLanguage")
            .and_then(Value::as_str)
            .and_then(|value| LanguageCode::new(value).ok());
        Ok(TranslationResponse {
            translations,
            detected_source_language,
        })
    }

    fn translate_with_microsoft(
        &self,
        execution: &TranslationExecutionConfig,
        request: &TranslationRequest,
    ) -> std::result::Result<TranslationResponse, TranslationError> {
        let config = provider_config(execution, TranslationProvider::Microsoft)?;
        let url = endpoint(&config, TranslationProvider::Microsoft, "/translate")?;
        let mut query = vec![
            ("api-version", "3.0".to_string()),
            ("to", microsoft_language(request.target_language.as_str())),
        ];
        if let Some(source) = request.source_language.as_ref() {
            query.push(("from", microsoft_language(source.as_str())));
        }
        let mut request_builder = self
            .client
            .post(url)
            .query(&query)
            .header("Ocp-Apim-Subscription-Key", config.api_key)
            .json(
                &request
                    .texts
                    .iter()
                    .map(|text| json!({ "Text": text }))
                    .collect::<Vec<_>>(),
            );
        if let Some(region) = config.region.as_deref() {
            request_builder = request_builder.header("Ocp-Apim-Subscription-Region", region);
        }
        let response = request_builder
            .send()
            .map_err(|error| request_error(TranslationProvider::Microsoft, error))?;
        let value = json_response(TranslationProvider::Microsoft, response)?;
        let translations = value
            .as_array()
            .ok_or_else(|| malformed_response(TranslationProvider::Microsoft))?
            .iter()
            .map(|value| {
                value
                    .pointer("/translations/0/text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| malformed_response(TranslationProvider::Microsoft))
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        validate_response_count(
            TranslationProvider::Microsoft,
            &translations,
            request.texts.len(),
        )?;
        let detected_source_language = value
            .get(0)
            .and_then(|value| value.get("detectedLanguage"))
            .and_then(|value| value.get("language"))
            .and_then(Value::as_str)
            .and_then(|value| LanguageCode::new(value).ok());
        Ok(TranslationResponse {
            translations,
            detected_source_language,
        })
    }
}

fn provider_config(
    config: &TranslationExecutionConfig,
    provider: TranslationProvider,
) -> std::result::Result<TranslationProviderConfig, TranslationError> {
    let config = config.provider_config(provider);
    if config.api_key.is_empty() {
        return Err(TranslationError {
            provider,
            category: TranslationErrorCategory::Authentication,
            message: format!("{} API key is missing", provider.label()),
            status: None,
            retryable: false,
            retry_after: None,
        });
    }
    Ok(config)
}

fn endpoint(
    config: &TranslationProviderConfig,
    provider: TranslationProvider,
    path: &str,
) -> std::result::Result<String, TranslationError> {
    let base_url = config
        .base_url
        .as_deref()
        .or_else(|| provider.default_base_url())
        .unwrap_or_default()
        .trim_end_matches('/');
    if !(base_url.starts_with("https://") || base_url.starts_with("http://localhost")) {
        return Err(TranslationError {
            provider,
            category: TranslationErrorCategory::InvalidRequest,
            message: "translation endpoint must use HTTPS, except localhost".to_string(),
            status: None,
            retryable: false,
            retry_after: None,
        });
    }
    Ok(format!("{base_url}{path}"))
}

fn sanitize_request(
    mut request: TranslationRequest,
) -> std::result::Result<TranslationRequest, anyhow::Error> {
    request.texts.retain(|text| !text.trim().is_empty());
    if request.texts.is_empty() {
        return Err(anyhow!("select text before translating"));
    }
    if request.texts.len() > MAX_BATCH_TEXTS
        || request.texts.iter().map(String::len).sum::<usize>() > MAX_BATCH_CHARS
    {
        return Err(anyhow!(
            "translation request exceeds the provider batch limit"
        ));
    }
    Ok(request)
}

fn split_batches(texts: Vec<String>) -> Vec<Vec<String>> {
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    let mut chars = 0;
    for text in texts {
        if !batch.is_empty()
            && (batch.len() >= MAX_BATCH_TEXTS || chars + text.len() > MAX_BATCH_CHARS)
        {
            batches.push(std::mem::take(&mut batch));
            chars = 0;
        }
        chars += text.len();
        batch.push(text);
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    batches
}

fn json_response(
    provider: TranslationProvider,
    response: reqwest::blocking::Response,
) -> std::result::Result<Value, TranslationError> {
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(glyphst_ai::translation::retry_after_duration);
    let body = response.text().map_err(|_| TranslationError {
        provider,
        category: TranslationErrorCategory::Network,
        message: format!("failed to read {} response", provider.label()),
        status: Some(status.as_u16()),
        retryable: true,
        retry_after: None,
    })?;
    if !status.is_success() {
        let category = match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                TranslationErrorCategory::Authentication
            }
            StatusCode::TOO_MANY_REQUESTS => TranslationErrorCategory::RateLimited,
            StatusCode::REQUEST_TIMEOUT => TranslationErrorCategory::Timeout,
            status if status.is_server_error() => TranslationErrorCategory::ServiceUnavailable,
            _ => TranslationErrorCategory::InvalidRequest,
        };
        return Err(TranslationError {
            provider,
            category,
            message: format!("{} request failed with status {status}", provider.label()),
            status: Some(status.as_u16()),
            retryable: matches!(
                status,
                StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
            ) || status.is_server_error(),
            retry_after,
        });
    }
    serde_json::from_str(&body).map_err(|_| TranslationError {
        provider,
        category: TranslationErrorCategory::InvalidResponse,
        message: format!("{} returned an invalid response", provider.label()),
        status: Some(status.as_u16()),
        retryable: false,
        retry_after: None,
    })
}

fn request_error(provider: TranslationProvider, error: reqwest::Error) -> TranslationError {
    TranslationError {
        provider,
        category: if error.is_timeout() {
            TranslationErrorCategory::Timeout
        } else {
            TranslationErrorCategory::Network
        },
        message: format!("{} request could not be completed", provider.label()),
        status: error.status().map(|status| status.as_u16()),
        retryable: error.is_timeout() || error.is_connect(),
        retry_after: None,
    }
}

fn malformed_response(provider: TranslationProvider) -> TranslationError {
    TranslationError {
        provider,
        category: TranslationErrorCategory::InvalidResponse,
        message: format!("{} returned an unexpected response", provider.label()),
        status: None,
        retryable: false,
        retry_after: None,
    }
}

fn validate_response_count(
    provider: TranslationProvider,
    translations: &[String],
    expected_count: usize,
) -> std::result::Result<(), TranslationError> {
    if translations.len() != expected_count {
        return Err(TranslationError {
            provider,
            category: TranslationErrorCategory::InvalidResponse,
            message: format!(
                "{} returned {} translations for {expected_count} inputs",
                provider.label(),
                translations.len()
            ),
            status: None,
            retryable: true,
            retry_after: None,
        });
    }
    Ok(())
}

fn deepl_language(language: &str) -> String {
    match language {
        "pt-br" => "PT-BR".to_string(),
        "pt-pt" => "PT-PT".to_string(),
        "zh" | "zh-cn" | "zh-hans" => "ZH".to_string(),
        "zh-tw" | "zh-hant" => "ZH-HANT".to_string(),
        _ => language.to_ascii_uppercase(),
    }
}

fn google_language(language: &str) -> String {
    match language {
        "zh-hans" => "zh-CN".to_string(),
        "zh-hant" => "zh-TW".to_string(),
        _ => language.to_string(),
    }
}

fn microsoft_language(language: &str) -> String {
    match language {
        "zh-cn" | "zh-hans" => "zh-Hans".to_string(),
        "zh-tw" | "zh-hant" => "zh-Hant".to_string(),
        _ => language.to_string(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreparedGoogleSemanticUnit {
    html: String,
    tokens: Vec<PreparedGoogleToken>,
    segments: Vec<GoogleSemanticSegment>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreparedGoogleInlineAnchoredUnit {
    prepared: PreparedGoogleSemanticUnit,
    chunks: Vec<GoogleInlineChunk>,
    anchors: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GoogleInlineChunk {
    unit: SemanticTranslationUnit,
    translatable: bool,
    leading_boundary: String,
    trailing_boundary: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreparedGoogleToken {
    placeholder: String,
    original: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum GoogleSemanticSegment {
    NaturalLanguage(String),
    Protected(String),
}

fn prepare_google_semantic_unit(
    unit: &SemanticTranslationUnit,
) -> std::result::Result<PreparedGoogleSemanticUnit, String> {
    let by_placeholder = unit
        .protected_tokens
        .iter()
        .map(|token| (token.placeholder.as_str(), token))
        .collect::<HashMap<_, _>>();
    if by_placeholder.len() != unit.protected_tokens.len() {
        return Err(format!(
            "Google semantic input {} contains duplicate protected-token metadata",
            unit.id
        ));
    }

    let mut html = String::with_capacity(unit.text.len());
    let mut tokens = Vec::with_capacity(unit.protected_tokens.len());
    let mut segments = Vec::with_capacity(unit.protected_tokens.len() * 2 + 1);
    let mut cursor = 0;
    while cursor < unit.text.len() {
        let remaining = &unit.text[cursor..];
        let Some(relative_start) = remaining.find("[[GLYPHST_") else {
            if !remaining.is_empty() {
                html.push_str(&html_escape(remaining));
                segments.push(GoogleSemanticSegment::NaturalLanguage(
                    remaining.to_string(),
                ));
            }
            break;
        };
        let start = cursor + relative_start;
        if start > cursor {
            let natural = &unit.text[cursor..start];
            html.push_str(&html_escape(natural));
            segments.push(GoogleSemanticSegment::NaturalLanguage(natural.to_string()));
        }
        let token_text = &unit.text[start..];
        let Some(relative_end) = token_text.find("]]") else {
            return Err(format!(
                "Google semantic input {} contains an incomplete placeholder",
                unit.id
            ));
        };
        let end = start + relative_end + 2;
        let placeholder = &unit.text[start..end];
        let Some(token) = by_placeholder.get(placeholder).copied() else {
            return Err(format!(
                "Google semantic input {} is missing metadata for {placeholder}",
                unit.id
            ));
        };
        let no_translate = matches!(
            token.kind,
            SemanticProtectedTokenKind::Formula
                | SemanticProtectedTokenKind::Citation
                | SemanticProtectedTokenKind::Url
                | SemanticProtectedTokenKind::StructuralMarker
                | SemanticProtectedTokenKind::InlineObject
                | SemanticProtectedTokenKind::LineBreak
                | SemanticProtectedTokenKind::StyleBoundary
        );
        if no_translate {
            html.push_str("<span translate=\"no\" class=\"notranslate\">");
        } else {
            html.push_str("<span>");
        }
        html.push_str(&html_escape(&token.original));
        html.push_str("</span>");
        tokens.push(PreparedGoogleToken {
            placeholder: token.placeholder.clone(),
            original: token.original.clone(),
        });
        segments.push(GoogleSemanticSegment::Protected(token.placeholder.clone()));
        cursor = end;
    }

    let observed = tokens
        .iter()
        .map(|token| token.placeholder.as_str())
        .collect::<HashSet<_>>();
    let unused = unit
        .protected_tokens
        .iter()
        .filter(|token| !observed.contains(token.placeholder.as_str()))
        .map(|token| token.placeholder.as_str())
        .collect::<Vec<_>>();
    if !unused.is_empty() {
        return Err(format!(
            "Google semantic input {} does not contain protected token(s): {}",
            unit.id,
            unused.join(", ")
        ));
    }

    Ok(PreparedGoogleSemanticUnit {
        html,
        tokens,
        segments,
    })
}

fn prepare_google_inline_anchored_unit(
    unit: &SemanticTranslationUnit,
) -> std::result::Result<PreparedGoogleInlineAnchoredUnit, String> {
    let prepared = prepare_google_semantic_unit(unit)?;
    let mut anchors = Vec::new();
    for token in unit
        .protected_tokens
        .iter()
        .filter(|token| token.kind == SemanticProtectedTokenKind::InlineObject)
    {
        let mut occurrences = unit.text.match_indices(&token.placeholder);
        let Some((start, _)) = occurrences.next() else {
            return Err(format!(
                "Google semantic input {} does not contain inline object {}",
                unit.id, token.placeholder
            ));
        };
        if occurrences.next().is_some() {
            return Err(format!(
                "Google semantic input {} contains inline object {} more than once",
                unit.id, token.placeholder
            ));
        }
        anchors.push((
            start,
            start + token.placeholder.len(),
            token.placeholder.clone(),
        ));
    }
    if anchors.is_empty() {
        return Err(format!(
            "Google semantic input {} has no inline-object anchors",
            unit.id
        ));
    }
    anchors.sort_by_key(|(start, _, _)| *start);

    let mut chunks = Vec::with_capacity(anchors.len() + 1);
    let mut cursor = 0usize;
    for (index, (start, end, _)) in anchors.iter().enumerate() {
        if *start < cursor {
            return Err(format!(
                "Google semantic input {} contains overlapping inline-object anchors",
                unit.id
            ));
        }
        chunks.push(prepare_google_inline_chunk(
            unit,
            index,
            &unit.text[cursor..*start],
        )?);
        cursor = *end;
    }
    chunks.push(prepare_google_inline_chunk(
        unit,
        anchors.len(),
        &unit.text[cursor..],
    )?);

    Ok(PreparedGoogleInlineAnchoredUnit {
        prepared,
        chunks,
        anchors: anchors
            .into_iter()
            .map(|(_, _, placeholder)| placeholder)
            .collect(),
    })
}

fn prepare_google_inline_chunk(
    unit: &SemanticTranslationUnit,
    index: usize,
    text: &str,
) -> std::result::Result<GoogleInlineChunk, String> {
    let chunk = SemanticTranslationUnit {
        id: format!("{}-inline-chunk-{index}", unit.id),
        role: unit.role.clone(),
        text: text.to_string(),
        protected_tokens: unit
            .protected_tokens
            .iter()
            .filter(|token| {
                token.kind != SemanticProtectedTokenKind::InlineObject
                    && text.contains(&token.placeholder)
            })
            .cloned()
            .collect(),
        context_before: None,
        context_after: None,
    };
    let prepared = prepare_google_semantic_unit(&chunk)?;
    let translatable = prepared.segments.iter().any(|segment| {
        matches!(
            segment,
            GoogleSemanticSegment::NaturalLanguage(text)
                if text.chars().any(char::is_alphabetic)
        )
    });
    let leading_boundary = google_natural_edge_boundary(&prepared, &chunk, true);
    let trailing_boundary = google_natural_edge_boundary(&prepared, &chunk, false);
    Ok(GoogleInlineChunk {
        unit: chunk,
        translatable,
        leading_boundary,
        trailing_boundary,
    })
}

fn restore_google_semantic_html(
    prepared: &PreparedGoogleSemanticUnit,
    translated_html: &str,
    strategy: &str,
) -> std::result::Result<String, String> {
    let mut restored = String::with_capacity(translated_html.len());
    let mut remaining = translated_html;
    let mut token_index = 0;
    let mut mismatches = Vec::new();
    let mut observed_spans = 0;

    while let Some(start) = remaining.find("<span") {
        restored.push_str(&html_unescape(&remaining[..start]));
        remaining = &remaining[start..];
        let Some(open_end) = remaining.find('>') else {
            mismatches.push("unterminated protected span".to_string());
            remaining = "";
            break;
        };
        let content_start = open_end + 1;
        let Some(close_start) = remaining[content_start..].find("</span>") else {
            mismatches.push("unterminated protected span".to_string());
            remaining = "";
            break;
        };
        let close_start = content_start + close_start;
        let actual = html_unescape(&remaining[content_start..close_start]);
        if let Some(expected) = prepared.tokens.get(token_index) {
            if actual == expected.original {
                restored.push_str(&expected.placeholder);
            } else {
                mismatches.push(format!(
                    "{} expected {:?}, found {:?}",
                    expected.placeholder,
                    diagnostic_excerpt(&expected.original),
                    diagnostic_excerpt(&actual)
                ));
            }
        } else {
            mismatches.push(format!(
                "unexpected protected span containing {:?}",
                diagnostic_excerpt(&actual)
            ));
        }
        observed_spans += 1;
        token_index += 1;
        remaining = &remaining[close_start + "</span>".len()..];
    }
    restored.push_str(&html_unescape(remaining));

    for missing in prepared.tokens.iter().skip(token_index) {
        mismatches.push(format!("{} was not returned", missing.placeholder));
    }
    if observed_spans != prepared.tokens.len() || !mismatches.is_empty() {
        return Err(format!(
            "{strategy} token mismatch: expected {} protected spans, found {observed_spans}; {}",
            prepared.tokens.len(),
            mismatches.join("; ")
        ));
    }
    Ok(restored)
}

fn validate_google_semantic_output(
    unit: &SemanticTranslationUnit,
    prepared: &PreparedGoogleSemanticUnit,
    translated: &str,
    source_language: Option<&LanguageCode>,
    target_language: &LanguageCode,
    strategy: &str,
) -> std::result::Result<(), String> {
    if !validate_semantic_cache_entry(unit, translated) {
        let expected = semantic_placeholders(&unit.text);
        let actual = semantic_placeholders(translated);
        let expected_set = expected.iter().copied().collect::<HashSet<_>>();
        let actual_set = actual.iter().copied().collect::<HashSet<_>>();
        let missing = expected
            .iter()
            .copied()
            .filter(|placeholder| !actual_set.contains(placeholder))
            .collect::<Vec<_>>()
            .join(", ");
        let unexpected = actual
            .iter()
            .copied()
            .filter(|placeholder| !expected_set.contains(placeholder))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "{strategy} did not restore the complete token set; missing [{missing}]; \
             unexpected [{unexpected}]"
        ));
    }
    if !prepared.tokens.is_empty()
        && translated.matches("[[GLYPHST_").count() != prepared.tokens.len()
    {
        return Err(format!(
            "{strategy} restored {} opaque tokens for {} protected values",
            translated.matches("[[GLYPHST_").count(),
            prepared.tokens.len()
        ));
    }
    let reject_unchanged = source_language.is_none_or(|source| source != target_language);
    let alphabetic = prepared
        .segments
        .iter()
        .filter_map(|segment| match segment {
            GoogleSemanticSegment::NaturalLanguage(text) => Some(text.as_str()),
            GoogleSemanticSegment::Protected(_) => None,
        })
        .flat_map(str::chars)
        .filter(|character| character.is_alphabetic())
        .count();
    if reject_unchanged
        && alphabetic >= 12
        && normalize_semantic_comparison(translated) == normalize_semantic_comparison(&unit.text)
    {
        return Err(format!(
            "{strategy} translation is unchanged from the source"
        ));
    }
    Ok(())
}

fn normalize_semantic_comparison(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_whitespace() && !character.is_ascii_punctuation())
        .flat_map(char::to_lowercase)
        .collect()
}

fn reassemble_google_segments(
    prepared: &PreparedGoogleSemanticUnit,
    translated_natural_language: &[String],
) -> String {
    let mut translated = translated_natural_language.iter();
    let mut result = String::new();
    for segment in &prepared.segments {
        match segment {
            GoogleSemanticSegment::NaturalLanguage(text)
                if text.chars().any(char::is_alphabetic) =>
            {
                result.push_str(translated.next().map(String::as_str).unwrap_or(text));
            }
            GoogleSemanticSegment::NaturalLanguage(text) => result.push_str(text),
            GoogleSemanticSegment::Protected(placeholder) => result.push_str(placeholder),
        }
    }
    result
}

fn restore_google_edge_whitespace(source: &str, translated: &str) -> String {
    let leading_end = source.len() - source.trim_start_matches(char::is_whitespace).len();
    let trailing_start = source
        .trim_end_matches(char::is_whitespace)
        .len()
        .max(leading_end);
    format!(
        "{}{}{}",
        &source[..leading_end],
        translated,
        &source[trailing_start..]
    )
}

fn google_natural_edge_boundary(
    prepared: &PreparedGoogleSemanticUnit,
    unit: &SemanticTranslationUnit,
    leading: bool,
) -> String {
    let style_boundaries = unit
        .protected_tokens
        .iter()
        .filter(|token| token.kind == SemanticProtectedTokenKind::StyleBoundary)
        .map(|token| token.placeholder.as_str())
        .collect::<HashSet<_>>();
    let mut boundary = String::new();
    if leading {
        for segment in &prepared.segments {
            match segment {
                GoogleSemanticSegment::Protected(placeholder)
                    if style_boundaries.contains(placeholder.as_str()) => {}
                GoogleSemanticSegment::Protected(_) => break,
                GoogleSemanticSegment::NaturalLanguage(text) => {
                    let edge_end =
                        text.len() - text.trim_start_matches(is_google_edge_character).len();
                    boundary.push_str(&text[..edge_end]);
                    if edge_end < text.len() {
                        break;
                    }
                }
            }
        }
    } else {
        for segment in prepared.segments.iter().rev() {
            match segment {
                GoogleSemanticSegment::Protected(placeholder)
                    if style_boundaries.contains(placeholder.as_str()) => {}
                GoogleSemanticSegment::Protected(_) => break,
                GoogleSemanticSegment::NaturalLanguage(text) => {
                    let edge_start = text.trim_end_matches(is_google_edge_character).len();
                    boundary.insert_str(0, &text[edge_start..]);
                    if edge_start > 0 {
                        break;
                    }
                }
            }
        }
    }
    if boundary.chars().any(is_google_edge_punctuation) {
        boundary
    } else {
        String::new()
    }
}

fn restore_google_edge_boundaries(
    unit: &SemanticTranslationUnit,
    translated: &str,
    target_language: &LanguageCode,
    leading_boundary: &str,
    trailing_boundary: &str,
) -> String {
    let mut restored = restore_google_edge_whitespace(&unit.text, translated);
    if !leading_boundary.is_empty() {
        let insertion = google_leading_content_index(&restored, unit);
        let starts_with_placeholder = restored[insertion..].starts_with("[[GLYPHST_");
        let translated_has_punctuation = !starts_with_placeholder
            && restored[insertion..]
                .chars()
                .next()
                .is_some_and(is_google_edge_punctuation);
        if !translated_has_punctuation {
            restored.insert_str(
                insertion,
                &localize_google_edge_punctuation(leading_boundary, target_language),
            );
        }
    }

    if !trailing_boundary.is_empty() {
        let insertion = google_trailing_content_index(&restored, unit);
        let ends_with_placeholder = unit
            .protected_tokens
            .iter()
            .any(|token| restored[..insertion].ends_with(&token.placeholder));
        let translated_has_punctuation = !ends_with_placeholder
            && restored[..insertion]
                .chars()
                .next_back()
                .is_some_and(is_google_edge_punctuation);
        if !translated_has_punctuation {
            restored.insert_str(
                insertion,
                &localize_google_edge_punctuation(trailing_boundary, target_language),
            );
        }
    }
    restored
}

fn google_leading_content_index(value: &str, unit: &SemanticTranslationUnit) -> usize {
    let mut cursor = 0usize;
    loop {
        cursor += value[cursor..].len().saturating_sub(
            value[cursor..]
                .trim_start_matches(char::is_whitespace)
                .len(),
        );
        let remaining = &value[cursor..];
        let Some(end) = remaining
            .starts_with("[[GLYPHST_")
            .then(|| remaining.find("]]"))
            .flatten()
            .map(|end| end + 2)
        else {
            break;
        };
        let placeholder = &remaining[..end];
        let style_boundary = unit.protected_tokens.iter().any(|token| {
            token.kind == SemanticProtectedTokenKind::StyleBoundary
                && token.placeholder == placeholder
        });
        if !style_boundary {
            break;
        }
        cursor += end;
    }
    cursor
}

fn google_trailing_content_index(value: &str, unit: &SemanticTranslationUnit) -> usize {
    let mut cursor = value.len();
    loop {
        cursor = value[..cursor].trim_end_matches(char::is_whitespace).len();
        let Some(token) = unit.protected_tokens.iter().find(|token| {
            token.kind == SemanticProtectedTokenKind::StyleBoundary
                && value[..cursor].ends_with(&token.placeholder)
        }) else {
            break;
        };
        cursor -= token.placeholder.len();
    }
    cursor
}

fn is_google_edge_character(character: char) -> bool {
    character.is_whitespace() || is_google_edge_punctuation(character)
}

fn is_google_edge_punctuation(character: char) -> bool {
    character.general_category_group() == GeneralCategoryGroup::Punctuation
}

fn localize_google_edge_punctuation(value: &str, target_language: &LanguageCode) -> String {
    let cjk = matches!(
        target_language.as_str(),
        "zh" | "zh-cn" | "zh-hans" | "zh-tw" | "zh-hant" | "ja"
    );
    value
        .chars()
        .filter(|character| !cjk || !character.is_whitespace())
        .map(|character| match (target_language.as_str(), character) {
            ("zh" | "zh-cn" | "zh-hans" | "zh-tw" | "zh-hant", '.') => '。',
            ("zh" | "zh-cn" | "zh-hans" | "zh-tw" | "zh-hant", ',') => '，',
            ("zh" | "zh-cn" | "zh-hans" | "zh-tw" | "zh-hant", ':') => '：',
            ("zh" | "zh-cn" | "zh-hans" | "zh-tw" | "zh-hant", ';') => '；',
            ("zh" | "zh-cn" | "zh-hans" | "zh-tw" | "zh-hant", '!') => '！',
            ("zh" | "zh-cn" | "zh-hans" | "zh-tw" | "zh-hant", '?') => '？',
            ("ja", '.') => '。',
            ("ja", ',') => '、',
            _ => character,
        })
        .collect()
}

fn diagnostic_excerpt(value: &str) -> String {
    const LIMIT: usize = 80;
    let mut excerpt = value.chars().take(LIMIT).collect::<String>();
    if value.chars().count() > LIMIT {
        excerpt.push('…');
    }
    excerpt
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn html_unescape(value: &str) -> String {
    let mut unescaped = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(relative) = value[cursor..].find('&') {
        let start = cursor + relative;
        unescaped.push_str(&value[cursor..start]);
        let remaining = &value[start..];
        let replacement = [
            ("&quot;", '"'),
            ("&#39;", '\''),
            ("&#x27;", '\''),
            ("&amp;", '&'),
            ("&lt;", '<'),
            ("&gt;", '>'),
        ]
        .into_iter()
        .find(|(entity, _)| remaining.starts_with(entity));
        if let Some((entity, character)) = replacement {
            unescaped.push(character);
            cursor = start + entity.len();
        } else {
            unescaped.push('&');
            cursor = start + 1;
        }
    }
    unescaped.push_str(&value[cursor..]);
    unescaped
}

fn cache_key(request: &TranslationRequest) -> String {
    let mut hasher = DefaultHasher::new();
    request.provider.hash(&mut hasher);
    request.source_language.hash(&mut hasher);
    request.target_language.hash(&mut hasher);
    request.texts.hash(&mut hasher);
    request.context_before.hash(&mut hasher);
    request.context_after.hash(&mut hasher);
    format!("v1-{:016x}", hasher.finish())
}

fn semantic_cache_key(
    config: &TranslationExecutionConfig,
    provider: TranslationProvider,
    source_language: Option<&LanguageCode>,
    target_language: &LanguageCode,
    unit: &SemanticTranslationUnit,
) -> String {
    let mut hasher = DefaultHasher::new();
    if provider == TranslationProvider::Google {
        GOOGLE_SEMANTIC_PROTOCOL_VERSION.hash(&mut hasher);
    } else {
        SEMANTIC_PROTOCOL_VERSION.hash(&mut hasher);
    }
    provider.hash(&mut hasher);
    semantic_provider_identity(config, provider).hash(&mut hasher);
    source_language.hash(&mut hasher);
    target_language.hash(&mut hasher);
    unit.role.hash(&mut hasher);
    unit.text.hash(&mut hasher);
    unit.protected_tokens.hash(&mut hasher);
    unit.context_before.hash(&mut hasher);
    unit.context_after.hash(&mut hasher);
    let prefix = if provider == TranslationProvider::Google {
        "google-semantic-v4"
    } else {
        "semantic-v5"
    };
    format!("{prefix}-{:016x}", hasher.finish())
}

fn semantic_provider_identity(
    config: &TranslationExecutionConfig,
    provider: TranslationProvider,
) -> String {
    if provider == TranslationProvider::Ai {
        let resolved = &config.ai;
        return format!(
            "{}|{}|{}",
            resolved.provider.as_str(),
            resolved.model.trim(),
            normalized_ai_endpoint(resolved)
        );
    }
    let config = config.provider_config(provider);
    format!(
        "{}|{}|{}",
        provider.as_str(),
        normalize_endpoint(
            config
                .base_url
                .as_deref()
                .or_else(|| provider.default_base_url())
                .unwrap_or_default()
        ),
        config.region.as_deref().unwrap_or_default().trim()
    )
}

fn normalize_endpoint(endpoint: &str) -> String {
    endpoint.trim().trim_end_matches('/').to_string()
}

fn normalized_ai_endpoint(settings: &glyphst_settings::ResolvedAiConfig) -> String {
    let base = normalize_endpoint(&settings.base_url);
    match settings.provider.api_style() {
        AiApiStyle::OpenAi if base.ends_with("/chat/completions") => base,
        AiApiStyle::OpenAi if base_url_has_version(&base) => format!("{base}/chat/completions"),
        AiApiStyle::OpenAi => format!("{base}/v1/chat/completions"),
        AiApiStyle::OpenAiResponses if base.ends_with("/responses") => base,
        AiApiStyle::OpenAiResponses => format!("{base}/responses"),
        AiApiStyle::Anthropic if base.ends_with("/v1/messages") => base,
        AiApiStyle::Anthropic if base.ends_with("/v1") => format!("{base}/messages"),
        AiApiStyle::Anthropic => format!("{base}/v1/messages"),
    }
}

fn base_url_has_version(base_url: &str) -> bool {
    base_url.rsplit('/').next().is_some_and(|segment| {
        segment.len() > 1
            && segment.starts_with('v')
            && segment[1..]
                .chars()
                .all(|character| character.is_ascii_digit())
    })
}

fn validate_semantic_cache_entry(unit: &SemanticTranslationUnit, translated: &str) -> bool {
    if translated.trim().is_empty() {
        return false;
    }
    let expected = semantic_placeholders(&unit.text);
    let actual = semantic_placeholders(translated);
    expected == actual && translated.matches("[[GLYPHST_").count() == actual.len()
}

fn semantic_placeholders(text: &str) -> Vec<&str> {
    let mut placeholders = Vec::new();
    let mut remaining = text;
    while let Some(start) = remaining.find("[[GLYPHST_") {
        remaining = &remaining[start..];
        let Some(end) = remaining.find("]]") else {
            break;
        };
        placeholders.push(&remaining[..end + 2]);
        remaining = &remaining[end + 2..];
    }
    placeholders.sort_unstable();
    placeholders
}

fn order_semantic_translations(
    units: &[SemanticTranslationUnit],
    translations: Vec<SemanticTranslation>,
    _provider: TranslationProvider,
) -> std::result::Result<Vec<SemanticTranslation>, TranslationError> {
    let mut by_id = HashMap::with_capacity(translations.len());
    let mut duplicate_ids = std::collections::HashSet::new();
    for translation in translations {
        let id = translation.id.clone();
        if by_id.insert(id.clone(), translation).is_some() {
            duplicate_ids.insert(id);
        }
    }
    for id in duplicate_ids {
        by_id.remove(&id);
    }
    Ok(units
        .iter()
        .filter_map(|unit| by_id.remove(&unit.id))
        .collect())
}

fn ai_translation_error(error: glyphst_ai::translation::AiTranslationError) -> TranslationError {
    use glyphst_ai::translation::AiTranslationErrorCategory as AiCategory;

    let category = match error.category {
        AiCategory::Authentication => TranslationErrorCategory::Authentication,
        AiCategory::RateLimited => TranslationErrorCategory::RateLimited,
        AiCategory::ServiceUnavailable => TranslationErrorCategory::ServiceUnavailable,
        AiCategory::Timeout => TranslationErrorCategory::Timeout,
        AiCategory::InvalidRequest => TranslationErrorCategory::InvalidRequest,
        AiCategory::InvalidResponse => TranslationErrorCategory::InvalidResponse,
        AiCategory::Network => TranslationErrorCategory::Network,
    };
    TranslationError {
        provider: TranslationProvider::Ai,
        category,
        message: error.message,
        status: error.status,
        retryable: error.retryable,
        retry_after: error.retry_after,
    }
}

mod cache;

use cache::{TranslationCache, unix_seconds};

#[cfg(test)]
mod tests;
