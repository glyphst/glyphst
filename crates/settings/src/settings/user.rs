//! The user's settings for hosts other than the GPUI apps: the config file
//! the GPUI apps write and, for provider secrets kept in the system
//! credential store, the same keychain items. Nothing is written here; the
//! GPUI settings store remains the editor of both.

use super::persistence::{config_path, load_settings, state_path};
use super::*;

/// The user's settings: the config file, else the legacy state file, else
/// defaults. Secrets kept in the keychain are empty until
/// [`read_keychain_secrets`].
pub fn load_user_settings() -> Settings {
    for path in [config_path(), state_path()] {
        if path.is_file() {
            match load_settings(&path) {
                Ok(settings) => return settings,
                Err(error) => {
                    eprintln!("failed to load settings from {}: {error:#}", path.display())
                }
            }
        }
    }
    Settings::default()
}

/// Secrets the settings do not hold themselves, so the GPUI apps keep them
/// in the system credential store.
pub fn keychain_secret_ids(settings: &Settings) -> Vec<ProviderSecretId> {
    settings
        .provider_secret_ids()
        .into_iter()
        .filter(|&id| settings.provider_secret(id).is_none_or(str::is_empty))
        .collect()
}

/// Reads the keychain secrets into `settings`, blocking; returns what could
/// not be read. Run it off the UI thread: unlocking the keyring may prompt.
pub fn read_keychain_secrets(settings: &mut Settings) -> Vec<String> {
    let ids = keychain_secret_ids(settings);
    if ids.is_empty() {
        return Vec::new();
    }
    match keychain::read(&ids) {
        Ok(found) => {
            for (id, secret) in found {
                settings.set_provider_secret(id, secret);
            }
            Vec::new()
        }
        Err(error) => vec![format!("system credential store: {error:#}")],
    }
}

#[cfg(all(feature = "keychain", target_os = "linux"))]
mod keychain {
    use super::*;

    /// The label GPUI gives the items it writes (gpui-pre-linux
    /// `KEYRING_LABEL`); the url attribute is the secret's credential URL.
    const GPUI_KEYRING_LABEL: &str = "zed-github-account";

    pub fn read(ids: &[ProviderSecretId]) -> Result<Vec<(ProviderSecretId, String)>> {
        pollster::block_on(async {
            let keyring = oo7::Keyring::new().await?;
            keyring.unlock().await?;
            let mut found = Vec::new();
            for &id in ids {
                let url = id.credential_url();
                for item in keyring.search_items(&vec![("url", url.as_str())]).await? {
                    if item
                        .label()
                        .await
                        .is_ok_and(|label| label == GPUI_KEYRING_LABEL)
                    {
                        item.unlock().await?;
                        let secret = item.secret().await?;
                        found.push((id, String::from_utf8_lossy(&secret).into_owned()));
                        break;
                    }
                }
            }
            Ok(found)
        })
    }
}

#[cfg(not(all(feature = "keychain", target_os = "linux")))]
mod keychain {
    use super::*;

    pub fn read(_: &[ProviderSecretId]) -> Result<Vec<(ProviderSecretId, String)>> {
        anyhow::bail!("not supported in this build")
    }
}

#[cfg(test)]
mod user_tests {
    use super::*;

    #[test]
    fn secrets_missing_from_the_config_are_read_from_the_keychain() {
        let settings = parse_settings(
            r#"
            translation {
                provider "deepl"
                providers {
                    deepl { api_key "from-config" }
                }
            }
            "#,
        )
        .unwrap();
        let ids = keychain_secret_ids(&settings);
        assert!(!ids.contains(&ProviderSecretId::TranslationApiKey(
            TranslationProvider::DeepL
        )));
        assert!(ids.contains(&ProviderSecretId::TranslationApiKey(
            TranslationProvider::Google
        )));
    }
}
