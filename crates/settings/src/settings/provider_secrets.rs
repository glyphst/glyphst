use super::{AiProvider, Settings, TranslationProvider};

const CREDENTIAL_URL_PREFIX: &str = "https://glyphst.app/credentials/providers/v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProviderSecretId {
    AiApiKey(AiProvider),
    TranslationApiKey(TranslationProvider),
    PaddleOcrToken,
    UnlimitedOcrApiKey,
    UnlimitedOcrSecretKey,
}

impl ProviderSecretId {
    pub fn credential_url(self) -> String {
        match self {
            Self::AiApiKey(provider) => {
                format!("{CREDENTIAL_URL_PREFIX}/ai/{}/api-key", provider.as_str())
            }
            Self::TranslationApiKey(provider) => format!(
                "{CREDENTIAL_URL_PREFIX}/translation/{}/api-key",
                provider.as_str()
            ),
            Self::PaddleOcrToken => {
                format!("{CREDENTIAL_URL_PREFIX}/ocr/paddle/token")
            }
            Self::UnlimitedOcrApiKey => {
                format!("{CREDENTIAL_URL_PREFIX}/ocr/unlimited/api-key")
            }
            Self::UnlimitedOcrSecretKey => {
                format!("{CREDENTIAL_URL_PREFIX}/ocr/unlimited/secret-key")
            }
        }
    }

    pub fn credential_username(self) -> &'static str {
        match self {
            Self::PaddleOcrToken => "Token",
            Self::UnlimitedOcrSecretKey => "Secret key",
            Self::AiApiKey(_) | Self::TranslationApiKey(_) | Self::UnlimitedOcrApiKey => "API key",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderSecretPersistence {
    Config,
    Keychain,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderSecretStatus {
    Loading,
    Config,
    Keychain,
    Missing,
    Saving(ProviderSecretPersistence),
    SessionOnly {
        intended: ProviderSecretPersistence,
        error: String,
    },
    Unavailable(String),
}

impl ProviderSecretStatus {
    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Loading)
    }

    pub fn source_label(&self) -> &'static str {
        match self {
            Self::Loading => "Loading from system credential store",
            Self::Config => "From config",
            Self::Keychain => "Stored in system credential store",
            Self::Missing => "Not configured",
            Self::Saving(ProviderSecretPersistence::Config) => "Saving to config",
            Self::Saving(ProviderSecretPersistence::Keychain) => {
                "Saving to system credential store"
            }
            Self::SessionOnly { .. } => "Not saved; available for this session only",
            Self::Unavailable(_) => "System credential store unavailable",
        }
    }

    pub fn error(&self) -> Option<&str> {
        match self {
            Self::SessionOnly { error, .. } | Self::Unavailable(error) => Some(error),
            _ => None,
        }
    }
}

/// Credential storage through GPUI's platform API, for the settings store.
#[cfg(feature = "gpui")]
mod backend {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use anyhow::Result;
    use gpui::{App, Task};

    pub(in crate::settings) type CredentialReadResult = Result<Option<(String, Vec<u8>)>>;

    pub(in crate::settings) trait ProviderCredentialBackend:
        Send + Sync
    {
        fn read(&self, url: &str, cx: &App) -> Task<CredentialReadResult>;
        fn write(&self, url: &str, username: &str, secret: &[u8], cx: &App) -> Task<Result<()>>;
        fn delete(&self, url: &str, cx: &App) -> Task<Result<()>>;
    }

    pub(in crate::settings) struct PlatformCredentialBackend;

    impl ProviderCredentialBackend for PlatformCredentialBackend {
        fn read(&self, url: &str, cx: &App) -> Task<CredentialReadResult> {
            cx.read_credentials(url)
        }

        fn write(&self, url: &str, username: &str, secret: &[u8], cx: &App) -> Task<Result<()>> {
            cx.write_credentials(url, username, secret)
        }

        fn delete(&self, url: &str, cx: &App) -> Task<Result<()>> {
            cx.delete_credentials(url)
        }
    }

    #[derive(Default)]
    pub(in crate::settings) struct MemoryCredentialBackend {
        entries: Mutex<HashMap<String, (String, Vec<u8>)>>,
    }

    impl ProviderCredentialBackend for MemoryCredentialBackend {
        fn read(&self, url: &str, _: &App) -> Task<CredentialReadResult> {
            let value = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(url)
                .cloned();
            Task::ready(Ok(value))
        }

        fn write(&self, url: &str, username: &str, secret: &[u8], _: &App) -> Task<Result<()>> {
            self.entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(url.to_string(), (username.to_string(), secret.to_vec()));
            Task::ready(Ok(()))
        }

        fn delete(&self, url: &str, _: &App) -> Task<Result<()>> {
            self.entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(url);
            Task::ready(Ok(()))
        }
    }

    pub(in crate::settings) fn platform_backend() -> Arc<dyn ProviderCredentialBackend> {
        Arc::new(PlatformCredentialBackend)
    }

    pub(in crate::settings) fn memory_backend() -> Arc<dyn ProviderCredentialBackend> {
        Arc::new(MemoryCredentialBackend::default())
    }
}
#[cfg(feature = "gpui")]
pub(super) use backend::*;

impl Settings {
    pub fn provider_secret_ids(&self) -> Vec<ProviderSecretId> {
        let mut ids = self
            .ai
            .provider_order
            .iter()
            .copied()
            .filter(|provider| *provider != AiProvider::ChatGptSubscription)
            .map(ProviderSecretId::AiApiKey)
            .collect::<Vec<_>>();
        ids.extend([
            ProviderSecretId::TranslationApiKey(TranslationProvider::DeepL),
            ProviderSecretId::TranslationApiKey(TranslationProvider::Google),
            ProviderSecretId::TranslationApiKey(TranslationProvider::Microsoft),
            ProviderSecretId::PaddleOcrToken,
            ProviderSecretId::UnlimitedOcrApiKey,
            ProviderSecretId::UnlimitedOcrSecretKey,
        ]);
        ids
    }

    pub fn provider_secret(&self, id: ProviderSecretId) -> Option<&str> {
        match id {
            ProviderSecretId::AiApiKey(provider) if provider != AiProvider::ChatGptSubscription => {
                self.ai
                    .providers
                    .get(&provider)
                    .map(|config| config.api_key.as_str())
            }
            ProviderSecretId::TranslationApiKey(provider)
                if provider != TranslationProvider::Ai =>
            {
                self.translation
                    .providers
                    .get(&provider)
                    .map(|config| config.api_key.as_str())
            }
            ProviderSecretId::PaddleOcrToken => Some(self.ocr.paddle.token.as_str()),
            ProviderSecretId::UnlimitedOcrApiKey => Some(self.ocr.unlimited.api_key.as_str()),
            ProviderSecretId::UnlimitedOcrSecretKey => Some(self.ocr.unlimited.secret_key.as_str()),
            _ => None,
        }
    }

    pub fn set_provider_secret(&mut self, id: ProviderSecretId, value: impl Into<String>) -> bool {
        let value = value.into();
        match id {
            ProviderSecretId::AiApiKey(provider) if provider != AiProvider::ChatGptSubscription => {
                let Some(config) = self.ai.providers.get_mut(&provider) else {
                    return false;
                };
                config.api_key = value;
            }
            ProviderSecretId::TranslationApiKey(provider)
                if provider != TranslationProvider::Ai =>
            {
                self.translation.provider_config_mut(provider).api_key = value;
            }
            ProviderSecretId::PaddleOcrToken => self.ocr.paddle.token = value,
            ProviderSecretId::UnlimitedOcrApiKey => self.ocr.unlimited.api_key = value,
            ProviderSecretId::UnlimitedOcrSecretKey => self.ocr.unlimited.secret_key = value,
            _ => return false,
        }
        true
    }

    pub fn copy_provider_secrets_from(&mut self, source: &Self) {
        for id in self.provider_secret_ids() {
            if let Some(secret) = source.provider_secret(id) {
                self.set_provider_secret(id, secret.to_string());
            }
        }
    }

    #[cfg(feature = "gpui")]
    pub(super) fn non_empty_provider_secrets(&self) -> Vec<(ProviderSecretId, String)> {
        self.provider_secret_ids()
            .into_iter()
            .filter_map(|id| {
                self.provider_secret(id)
                    .filter(|secret| !secret.is_empty())
                    .map(|secret| (id, secret.to_string()))
            })
            .collect()
    }

    #[cfg(feature = "gpui")]
    pub(super) fn without_provider_secrets(&self) -> Self {
        let mut settings = self.clone();
        for id in settings.provider_secret_ids() {
            settings.set_provider_secret(id, String::new());
        }
        settings
    }
}
