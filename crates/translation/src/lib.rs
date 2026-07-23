use std::{
    collections::{HashMap, HashSet},
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, anyhow};
use glyphst_settings::{
    AiApiStyle, Settings, TranslationProvider, TranslationProviderConfig, TranslationSettings,
};
use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use reqwest::{StatusCode, blocking::Client};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const CACHE_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("translations_v1");
const MAX_CACHE_ENTRIES: usize = 10_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_BATCH_TEXTS: usize = 64;
const MAX_BATCH_CHARS: usize = 24_000;
const SEMANTIC_PROTOCOL_VERSION: &str = "semantic-v5-json-items-adjacency-styles-structured-lines";
const GOOGLE_SEMANTIC_PROTOCOL_VERSION: &str = "google-semantic-v3-html-spans-segmented-v1";

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

impl gpui::Global for TranslationService {}

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
        settings: &Settings,
        request: TranslationRequest,
    ) -> std::result::Result<TranslationResponse, TranslationError> {
        let request = sanitize_request(request).map_err(|error| TranslationError {
            provider: settings.translation.active_provider,
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

        let response = self.request_provider(settings, &request)?;
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
        settings: &Settings,
        provider: TranslationProvider,
        source_language: Option<LanguageCode>,
        target_language: LanguageCode,
        texts: Vec<String>,
    ) -> std::result::Result<Vec<String>, TranslationError> {
        let mut translated = Vec::with_capacity(texts.len());
        for texts in split_batches(texts) {
            let batch = texts.clone();
            let response = self.translate(
                settings,
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
                            settings,
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
        settings: &Settings,
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
                    settings,
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
                settings,
                source_language.as_ref(),
                &target_language,
                &missing,
            )?,
            TranslationProvider::Google => self.translate_semantic_with_google(
                settings,
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
                let response = self.request_provider(settings, &request)?;
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
        settings: &Settings,
        provider: TranslationProvider,
        source_language: Option<&LanguageCode>,
        target_language: &LanguageCode,
        unit: &SemanticTranslationUnit,
        translated_text: &str,
    ) {
        self.cache_validated_semantic_translations(
            settings,
            provider,
            source_language,
            target_language,
            &[(unit.clone(), translated_text.to_string())],
        );
    }

    pub fn cache_validated_semantic_translations(
        &self,
        settings: &Settings,
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
                    semantic_cache_key(settings, provider, source_language, target_language, unit),
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
        settings: &Settings,
        source_language: Option<&LanguageCode>,
        target_language: &LanguageCode,
        unit: SemanticTranslationUnit,
    ) -> std::result::Result<SemanticTranslation, TranslationError> {
        if !settings
            .translation
            .provider_config(TranslationProvider::Ai)
            .enabled
        {
            return Err(TranslationError {
                provider: TranslationProvider::Ai,
                category: TranslationErrorCategory::InvalidRequest,
                message: "AI translation is disabled in settings".to_string(),
                status: None,
                retryable: false,
                retry_after: None,
            });
        }
        let resolved = settings.ai.resolved_translation();
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
        settings: &Settings,
        request: &TranslationRequest,
    ) -> std::result::Result<TranslationResponse, TranslationError> {
        match request.provider {
            TranslationProvider::Ai => self.translate_with_ai(settings, request),
            TranslationProvider::DeepL => self.translate_with_deepl(settings, request),
            TranslationProvider::Google => self.translate_with_google(settings, request),
            TranslationProvider::Microsoft => self.translate_with_microsoft(settings, request),
        }
    }

    fn translate_semantic_with_ai(
        &self,
        settings: &Settings,
        source_language: Option<&LanguageCode>,
        target_language: &LanguageCode,
        units: &[SemanticTranslationUnit],
    ) -> std::result::Result<Vec<SemanticTranslation>, TranslationError> {
        if !settings
            .translation
            .provider_config(TranslationProvider::Ai)
            .enabled
        {
            return Err(TranslationError {
                provider: TranslationProvider::Ai,
                category: TranslationErrorCategory::InvalidRequest,
                message: "AI translation is disabled in settings".to_string(),
                status: None,
                retryable: false,
                retry_after: None,
            });
        }
        let resolved = settings.ai.resolved_translation();
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
        settings: &Settings,
        source_language: Option<&LanguageCode>,
        target_language: &LanguageCode,
        units: &[SemanticTranslationUnit],
    ) -> std::result::Result<Vec<SemanticTranslation>, TranslationError> {
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
            settings,
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
                settings,
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
                settings,
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

    fn translate_google_semantic_segments(
        &self,
        settings: &Settings,
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
            settings,
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
        settings: &Settings,
        request: &TranslationRequest,
    ) -> std::result::Result<TranslationResponse, TranslationError> {
        if !settings
            .translation
            .provider_config(TranslationProvider::Ai)
            .enabled
        {
            return Err(TranslationError {
                provider: TranslationProvider::Ai,
                category: TranslationErrorCategory::InvalidRequest,
                message: "AI translation is disabled in settings".to_string(),
                status: None,
                retryable: false,
                retry_after: None,
            });
        }
        let resolved = settings.ai.resolved_translation();
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
        settings: &Settings,
        request: &TranslationRequest,
    ) -> std::result::Result<TranslationResponse, TranslationError> {
        let config = provider_config(&settings.translation, TranslationProvider::DeepL)?;
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
        settings: &Settings,
        request: &TranslationRequest,
    ) -> std::result::Result<TranslationResponse, TranslationError> {
        let mut response = self.request_google(
            settings,
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
        settings: &Settings,
        source_language: Option<&LanguageCode>,
        target_language: &LanguageCode,
        texts: Vec<String>,
        format: &str,
    ) -> std::result::Result<TranslationResponse, TranslationError> {
        let config = provider_config(&settings.translation, TranslationProvider::Google)?;
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
        settings: &Settings,
        request: &TranslationRequest,
    ) -> std::result::Result<TranslationResponse, TranslationError> {
        let config = provider_config(&settings.translation, TranslationProvider::Microsoft)?;
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
    settings: &TranslationSettings,
    provider: TranslationProvider,
) -> std::result::Result<TranslationProviderConfig, TranslationError> {
    let config = settings.provider_config(provider);
    if !config.enabled {
        return Err(TranslationError {
            provider,
            category: TranslationErrorCategory::InvalidRequest,
            message: format!("{} translation is disabled in settings", provider.label()),
            status: None,
            retryable: false,
            retry_after: None,
        });
    }
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
    let alphabetic = unit
        .text
        .chars()
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
    settings: &Settings,
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
    semantic_provider_identity(settings, provider).hash(&mut hasher);
    source_language.hash(&mut hasher);
    target_language.hash(&mut hasher);
    unit.role.hash(&mut hasher);
    unit.text.hash(&mut hasher);
    unit.protected_tokens.hash(&mut hasher);
    unit.context_before.hash(&mut hasher);
    unit.context_after.hash(&mut hasher);
    let prefix = if provider == TranslationProvider::Google {
        "google-semantic-v3"
    } else {
        "semantic-v5"
    };
    format!("{prefix}-{:016x}", hasher.finish())
}

fn semantic_provider_identity(settings: &Settings, provider: TranslationProvider) -> String {
    if provider == TranslationProvider::Ai {
        let resolved = settings.ai.resolved_translation();
        return format!(
            "{}|{}|{}",
            resolved.provider.as_str(),
            resolved.model.trim(),
            normalized_ai_endpoint(&resolved)
        );
    }
    let config = settings.translation.provider_config(provider);
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

struct TranslationCache {
    database: Option<Database>,
    memory: HashMap<String, CachedTranslation>,
}

impl TranslationCache {
    fn memory() -> Self {
        Self {
            database: None,
            memory: HashMap::new(),
        }
    }

    fn open() -> Result<Self> {
        let path = glyphst_core::paths::state_dir().join("translations.redb");
        Self::open_at(&path)
    }

    fn open_at(path: &std::path::Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            create_private_cache_directory(parent)?;
        }
        if !path.exists() {
            create_private_cache_file(path)?;
        }
        let database =
            Database::create(path).with_context(|| format!("failed to open {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("failed to restrict {}", path.display()))?;
        }
        let write_txn = database.begin_write()?;
        write_txn.open_table(CACHE_TABLE)?;
        write_txn.commit()?;
        Ok(Self {
            database: Some(database),
            ..Self::memory()
        })
    }

    fn clear(&mut self) -> Result<()> {
        if let Some(database) = &self.database {
            let write_txn = database.begin_write()?;
            {
                let mut table = write_txn.open_table(CACHE_TABLE)?;
                table.retain(|_, _| false)?;
            }
            write_txn.commit()?;
        }
        self.memory.clear();
        Ok(())
    }

    fn get(&mut self, key: &str) -> Option<CachedTranslation> {
        if let Some(value) = self.memory.get(key) {
            return Some(CachedTranslation {
                translations: value.translations.clone(),
                detected_source_language: value.detected_source_language.clone(),
                updated_at: value.updated_at,
            });
        }
        let database = self.database.as_ref()?;
        let read_txn = database.begin_read().ok()?;
        let table = read_txn.open_table(CACHE_TABLE).ok()?;
        let value = table.get(key).ok()??;
        let cached = bincode::deserialize::<CachedTranslation>(value.value()).ok()?;
        self.memory.insert(
            key.to_string(),
            CachedTranslation {
                translations: cached.translations.clone(),
                detected_source_language: cached.detected_source_language.clone(),
                updated_at: cached.updated_at,
            },
        );
        self.prune_memory_cache();
        Some(cached)
    }

    fn insert(&mut self, key: &str, value: CachedTranslation) -> Result<()> {
        self.insert_many([(key.to_string(), value)])
    }

    fn insert_many(
        &mut self,
        entries: impl IntoIterator<Item = (String, CachedTranslation)>,
    ) -> Result<()> {
        let entries = entries.into_iter().collect::<Vec<_>>();
        if entries.is_empty() {
            return Ok(());
        }
        for (key, value) in &entries {
            self.memory.insert(
                key.clone(),
                CachedTranslation {
                    translations: value.translations.clone(),
                    detected_source_language: value.detected_source_language.clone(),
                    updated_at: value.updated_at,
                },
            );
        }
        self.prune_memory_cache();
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(CACHE_TABLE)?;
            for (key, value) in entries {
                let encoded = bincode::serialize(&value)?;
                table.insert(key.as_str(), encoded.as_slice())?;
            }
            if table.len()? > MAX_CACHE_ENTRIES as u64 {
                prune_cache_table(&mut table)?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    fn prune_memory_cache(&mut self) {
        if self.memory.len() <= MAX_CACHE_ENTRIES {
            return;
        }
        let mut entries = self
            .memory
            .iter()
            .map(|(key, value)| (key.clone(), value.updated_at))
            .collect::<Vec<_>>();
        entries.sort_by_key(|(_, updated_at)| *updated_at);
        for (key, _) in entries
            .into_iter()
            .take(self.memory.len() - MAX_CACHE_ENTRIES)
        {
            self.memory.remove(&key);
        }
    }
}

fn create_private_cache_directory(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to restrict {}", path.display()))?;
    }

    #[cfg(not(unix))]
    {
        fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    }

    Ok(())
}

fn create_private_cache_file(path: &std::path::Path) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    Ok(())
}

fn prune_cache_table(table: &mut redb::Table<'_, &str, &[u8]>) -> Result<()> {
    let mut entries = table
        .iter()?
        .filter_map(|entry| {
            let (key, value) = entry.ok()?;
            let cached = bincode::deserialize::<CachedTranslation>(value.value()).ok()?;
            Some((key.value().to_string(), cached.updated_at))
        })
        .collect::<Vec<_>>();
    if entries.len() <= MAX_CACHE_ENTRIES {
        return Ok(());
    }
    entries.sort_by_key(|(_, updated_at)| *updated_at);
    let remove_count = entries.len() - MAX_CACHE_ENTRIES;
    for (key, _) in entries.into_iter().take(remove_count) {
        table.remove(key.as_str())?;
    }
    Ok(())
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};

    use super::*;
    use glyphst_settings::ModelRef;

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

        let error =
            restore_google_semantic_html(&prepared, &corrupted, "singleton HTML").unwrap_err();

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
        let key = semantic_cache_key(&settings, TranslationProvider::Google, None, &target, &unit);
        let mut changed_token = unit.clone();
        changed_token.protected_tokens[0].original = "ML".to_string();

        assert!(key.starts_with("google-semantic-v3-"));
        assert_ne!(
            key,
            semantic_cache_key(
                &settings,
                TranslationProvider::Google,
                None,
                &target,
                &changed_token
            )
        );
        assert!(!key.starts_with("semantic-v3-"));
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
        google.enabled = true;
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
                &settings,
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
        let path = directory.join("translations.redb");
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
        let base_key = semantic_cache_key(&settings, provider, None, &target, &unit);

        let mut changed_context = unit.clone();
        changed_context.context_before = Some("Different".to_string());
        assert_ne!(
            base_key,
            semantic_cache_key(&settings, provider, None, &target, &changed_context)
        );

        let mut changed_model = settings.clone();
        let ai_provider = changed_model.ai.resolved_translation().provider;
        changed_model.ai.translation_model = Some(ModelRef::new(ai_provider, "other-model"));
        assert_ne!(
            base_key,
            semantic_cache_key(&changed_model, provider, None, &target, &unit)
        );

        let mut changed_endpoint = settings.clone();
        changed_endpoint
            .ai
            .provider_config_mut(ai_provider)
            .base_url = Some("https://translation.example.test/v1/".to_string());
        assert_ne!(
            base_key,
            semantic_cache_key(&changed_endpoint, provider, None, &target, &unit)
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
        let key = semantic_cache_key(&settings, provider, None, &target, &unit);

        service.cache_validated_semantic_translation(
            &settings, provider, None, &target, &unit, "Valeur",
        );
        assert!(service.cache.lock().unwrap().get(&key).is_none());

        service.cache_validated_semantic_translation(
            &settings,
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
            &settings,
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
}
