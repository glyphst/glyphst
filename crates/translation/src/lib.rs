use std::{
    collections::HashMap,
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, anyhow};
use glyphst_settings::{
    Settings, TranslationProvider, TranslationProviderConfig, TranslationSettings,
};
use redb::{Database, ReadableTable, TableDefinition};
use reqwest::{StatusCode, blocking::Client};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const CACHE_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("translations_v1");
const MAX_CACHE_ENTRIES: usize = 10_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_BATCH_TEXTS: usize = 64;
const MAX_BATCH_CHARS: usize = 24_000;

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
    pub message: String,
    pub retryable: bool,
    pub retry_after: Option<Duration>,
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
        for (key, value) in cache.memory.clone() {
            persistent_cache.insert(&key, value)?;
        }
        *cache = persistent_cache;
        Ok(())
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
            message: error.to_string(),
            retryable: false,
            retry_after: None,
        })?;
        let cache_key = cache_key(&request);
        if let Some(cached) = self
            .cache
            .lock()
            .map_err(|_| TranslationError {
                provider: request.provider,
                message: "translation cache is unavailable".to_string(),
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
                message: "AI translation is disabled in settings".to_string(),
                retryable: false,
                retry_after: None,
            });
        }
        let resolved = settings.ai.resolved_translation();
        if !resolved.can_request() {
            return Err(TranslationError {
                provider: TranslationProvider::Ai,
                message: resolved.missing_credentials_message(),
                retryable: false,
                retry_after: None,
            });
        }
        let translations = glyphst_ai::translation::translate_batch(
            resolved,
            &request.texts,
            request.source_language.as_ref().map(LanguageCode::as_str),
            request.target_language.as_str(),
        )
        .map_err(|_| TranslationError {
            provider: TranslationProvider::Ai,
            message: "AI translation request failed".to_string(),
            retryable: true,
            retry_after: None,
        })?;
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
        let config = provider_config(&settings.translation, TranslationProvider::Google)?;
        let url = endpoint(
            &config,
            TranslationProvider::Google,
            "/language/translate/v2",
        )?;
        let mut body = json!({
            "q": request.texts,
            "target": google_language(request.target_language.as_str()),
            "format": "text",
        });
        if let Some(source) = request.source_language.as_ref() {
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
                    .map(html_unescape)
                    .ok_or_else(|| malformed_response(TranslationProvider::Google))
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        validate_response_count(
            TranslationProvider::Google,
            &translations,
            request.texts.len(),
        )?;
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
            message: format!("{} translation is disabled in settings", provider.label()),
            retryable: false,
            retry_after: None,
        });
    }
    if config.api_key.is_empty() {
        return Err(TranslationError {
            provider,
            message: format!("{} API key is missing", provider.label()),
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
            message: "translation endpoint must use HTTPS, except localhost".to_string(),
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
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs);
    let body = response.text().map_err(|_| TranslationError {
        provider,
        message: format!("failed to read {} response", provider.label()),
        retryable: true,
        retry_after: None,
    })?;
    if !status.is_success() {
        return Err(TranslationError {
            provider,
            message: format!("{} request failed with status {status}", provider.label()),
            retryable: status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error(),
            retry_after,
        });
    }
    serde_json::from_str(&body).map_err(|_| TranslationError {
        provider,
        message: format!("{} returned an invalid response", provider.label()),
        retryable: false,
        retry_after: None,
    })
}

fn request_error(provider: TranslationProvider, error: reqwest::Error) -> TranslationError {
    TranslationError {
        provider,
        message: format!("{} request could not be completed", provider.label()),
        retryable: error.is_timeout() || error.is_connect(),
        retry_after: None,
    }
}

fn malformed_response(provider: TranslationProvider) -> TranslationError {
    TranslationError {
        provider,
        message: format!("{} returned an unexpected response", provider.label()),
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
            message: format!(
                "{} returned {} translations for {expected_count} inputs",
                provider.label(),
                translations.len()
            ),
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

fn html_unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
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
        if let Some(parent) = path.parent() {
            create_private_cache_directory(parent)?;
        }
        if !path.exists() {
            create_private_cache_file(&path)?;
        }
        let database = Database::create(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
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
        self.memory.insert(
            key.to_string(),
            CachedTranslation {
                translations: value.translations.clone(),
                detected_source_language: value.detected_source_language.clone(),
                updated_at: value.updated_at,
            },
        );
        self.prune_memory_cache();
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(CACHE_TABLE)?;
            let encoded = bincode::serialize(&value)?;
            table.insert(key, encoded.as_slice())?;
            prune_cache_table(&mut table)?;
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
    use super::*;

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
}
