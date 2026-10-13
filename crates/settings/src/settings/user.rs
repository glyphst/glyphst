//! The user's settings for hosts other than the GPUI apps: the config file
//! the GPUI apps write and, for provider secrets kept in the system
//! credential store, the same keychain items, read and written the way
//! GPUI does so both apps see the same keys.
//!
//! Such a host edits only the provider sections (`ai`, `translation`,
//! `ocr`); saving re-reads the file and replaces just those, so what the
//! GPUI apps wrote elsewhere survives. A GPUI app running at the same time
//! keeps its own copy and writes it back on its next save.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

use super::persistence::{config_path, load_settings, state_path, write_kdl};
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

/// Secrets the config file holds (as loaded, before the keychain is read).
/// The GPUI apps keep writing these to the file; every other secret goes
/// to the keychain.
pub fn config_owned_secret_ids(settings: &Settings) -> BTreeSet<ProviderSecretId> {
    settings
        .provider_secret_ids()
        .into_iter()
        .filter(|&id| settings.provider_secret(id).is_some_and(|s| !s.is_empty()))
        .collect()
}

/// Where provider secrets outside the config file are kept.
pub trait CredentialStore: Send + Sync {
    /// The secrets found among `ids`.
    fn read(&self, ids: &[ProviderSecretId]) -> Result<Vec<(ProviderSecretId, String)>>;
    fn write(&self, id: ProviderSecretId, secret: &str) -> Result<()>;
    fn delete(&self, id: ProviderSecretId) -> Result<()>;
    /// Another item (a sign-in's credentials), by its URL.
    fn read_item(&self, url: &str) -> Result<Option<Vec<u8>>>;
    fn write_item(&self, url: &str, username: &str, secret: &[u8]) -> Result<()>;
    fn delete_item(&self, url: &str) -> Result<()>;
}

/// The system credential store (Secret Service on Linux), with the items
/// GPUI's platform writes. Each call blocks; unlocking may prompt.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemKeychain;

impl CredentialStore for SystemKeychain {
    fn read(&self, ids: &[ProviderSecretId]) -> Result<Vec<(ProviderSecretId, String)>> {
        keychain::read(ids)
    }

    fn write(&self, id: ProviderSecretId, secret: &str) -> Result<()> {
        keychain::write(id, secret)
    }

    fn delete(&self, id: ProviderSecretId) -> Result<()> {
        keychain::delete_item(&id.credential_url())
    }

    fn read_item(&self, url: &str) -> Result<Option<Vec<u8>>> {
        keychain::read_item(url)
    }

    fn write_item(&self, url: &str, username: &str, secret: &[u8]) -> Result<()> {
        keychain::write_item(url, username, secret)
    }

    fn delete_item(&self, url: &str) -> Result<()> {
        keychain::delete_item(url)
    }
}

/// Secrets kept in memory, for tests and scripted runs.
#[derive(Debug, Default)]
pub struct MemoryCredentials {
    secrets: Mutex<BTreeMap<ProviderSecretId, String>>,
    items: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl MemoryCredentials {
    pub fn get(&self, id: ProviderSecretId) -> Option<String> {
        self.secrets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .cloned()
    }
}

impl CredentialStore for MemoryCredentials {
    fn read(&self, ids: &[ProviderSecretId]) -> Result<Vec<(ProviderSecretId, String)>> {
        Ok(ids
            .iter()
            .filter_map(|&id| self.get(id).map(|secret| (id, secret)))
            .collect())
    }

    fn write(&self, id: ProviderSecretId, secret: &str) -> Result<()> {
        self.secrets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, secret.to_string());
        Ok(())
    }

    fn delete(&self, id: ProviderSecretId) -> Result<()> {
        self.secrets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&id);
        Ok(())
    }

    fn read_item(&self, url: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .items
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(url)
            .cloned())
    }

    fn write_item(&self, url: &str, _username: &str, secret: &[u8]) -> Result<()> {
        self.items
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(url.to_string(), secret.to_vec());
        Ok(())
    }

    fn delete_item(&self, url: &str) -> Result<()> {
        self.items
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(url);
        Ok(())
    }
}

/// Reads the keychain secrets into `settings`, blocking; returns what could
/// not be read. Run it off the UI thread: unlocking the keyring may prompt.
pub fn read_keychain_secrets(settings: &mut Settings, store: &dyn CredentialStore) -> Vec<String> {
    let ids = keychain_secret_ids(settings);
    if ids.is_empty() {
        return Vec::new();
    }
    match store.read(&ids) {
        Ok(found) => {
            for (id, secret) in found {
                settings.set_provider_secret(id, secret);
            }
            Vec::new()
        }
        Err(error) => vec![format!("system credential store: {error:#}")],
    }
}

/// [`save_user_settings_to`] the config file.
pub fn save_user_settings(
    edited: &Settings,
    config_owned: &BTreeSet<ProviderSecretId>,
) -> Result<()> {
    save_user_settings_to(&config_path(), edited, config_owned)
}

/// Writes `edited`'s provider sections (`ai`, `translation`, `ocr`) into the
/// settings file at `path`, keeping everything else as the file has it now
/// (or as the legacy state file has it, when there is no config yet).
/// Secrets outside `config_owned` are left out: they belong in the
/// keychain. The file is replaced atomically, readable by the user only.
pub fn save_user_settings_to(
    path: &Path,
    edited: &Settings,
    config_owned: &BTreeSet<ProviderSecretId>,
) -> Result<()> {
    let mut settings = if path.is_file() {
        load_settings(path)?
    } else if path == config_path() && state_path().is_file() {
        load_settings(&state_path())?
    } else {
        Settings::default()
    };
    settings.ai = edited.ai.clone();
    settings.translation = edited.translation.clone();
    settings.ocr = edited.ocr.clone();
    for id in settings.provider_secret_ids() {
        if !config_owned.contains(&id) {
            settings.set_provider_secret(id, String::new());
        }
    }
    settings.sanitize();
    write_kdl(path, settings.to_kdl_string())
}

#[cfg(all(feature = "keychain", target_os = "linux"))]
mod keychain {
    use super::*;

    /// The label GPUI gives the items it writes (gpui-pre-linux
    /// `KEYRING_LABEL`); the url attribute is the secret's credential URL.
    const GPUI_KEYRING_LABEL: &str = "zed-github-account";

    async fn unlocked() -> Result<oo7::Keyring> {
        let keyring = oo7::Keyring::new().await?;
        keyring.unlock().await?;
        Ok(keyring)
    }

    async fn gpui_items(keyring: &oo7::Keyring, url: &str) -> Result<Vec<oo7::Item>> {
        let mut found = Vec::new();
        for item in keyring.search_items(&vec![("url", url)]).await? {
            if item
                .label()
                .await
                .is_ok_and(|label| label == GPUI_KEYRING_LABEL)
            {
                found.push(item);
            }
        }
        Ok(found)
    }

    pub fn read(ids: &[ProviderSecretId]) -> Result<Vec<(ProviderSecretId, String)>> {
        pollster::block_on(async {
            let keyring = unlocked().await?;
            let mut found = Vec::new();
            for &id in ids {
                let url = id.credential_url();
                if let Some(item) = gpui_items(&keyring, &url).await?.into_iter().next() {
                    item.unlock().await?;
                    let secret = item.secret().await?;
                    found.push((id, String::from_utf8_lossy(&secret).into_owned()));
                }
            }
            Ok(found)
        })
    }

    pub fn write(id: ProviderSecretId, secret: &str) -> Result<()> {
        write_item(
            &id.credential_url(),
            id.credential_username(),
            secret.as_bytes(),
        )
    }

    /// As gpui-pre-linux `read_credentials`: the first item for the URL.
    pub fn read_item(url: &str) -> Result<Option<Vec<u8>>> {
        pollster::block_on(async {
            let keyring = unlocked().await?;
            match gpui_items(&keyring, url).await?.into_iter().next() {
                Some(item) => {
                    item.unlock().await?;
                    Ok(Some(item.secret().await?.to_vec()))
                }
                None => Ok(None),
            }
        })
    }

    /// As gpui-pre-linux `write_credentials`: one item per URL, replaced.
    pub fn write_item(url: &str, username: &str, secret: &[u8]) -> Result<()> {
        pollster::block_on(async {
            let keyring = unlocked().await?;
            keyring
                .create_item(
                    GPUI_KEYRING_LABEL,
                    &vec![("url", url), ("username", username)],
                    secret.to_vec(),
                    true,
                )
                .await?;
            Ok(())
        })
    }

    /// As gpui-pre-linux `delete_credentials`.
    pub fn delete_item(url: &str) -> Result<()> {
        pollster::block_on(async {
            let keyring = unlocked().await?;
            for item in gpui_items(&keyring, url).await? {
                item.delete().await?;
            }
            Ok(())
        })
    }
}

#[cfg(not(all(feature = "keychain", target_os = "linux")))]
mod keychain {
    use super::*;

    pub fn read(_: &[ProviderSecretId]) -> Result<Vec<(ProviderSecretId, String)>> {
        anyhow::bail!("not supported in this build")
    }

    pub fn write(_: ProviderSecretId, _: &str) -> Result<()> {
        anyhow::bail!("not supported in this build")
    }

    pub fn read_item(_: &str) -> Result<Option<Vec<u8>>> {
        anyhow::bail!("not supported in this build")
    }

    pub fn write_item(_: &str, _: &str, _: &[u8]) -> Result<()> {
        anyhow::bail!("not supported in this build")
    }

    pub fn delete_item(_: &str) -> Result<()> {
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
        assert_eq!(
            config_owned_secret_ids(&settings),
            [ProviderSecretId::TranslationApiKey(
                TranslationProvider::DeepL
            )]
            .into()
        );

        let store = MemoryCredentials::default();
        let google = ProviderSecretId::TranslationApiKey(TranslationProvider::Google);
        store.write(google, "from-keychain").unwrap();
        let mut settings = settings;
        assert!(read_keychain_secrets(&mut settings, &store).is_empty());
        assert_eq!(settings.provider_secret(google), Some("from-keychain"));
        store.delete(google).unwrap();
        assert_eq!(store.get(google), None);
    }

    #[test]
    fn saving_replaces_only_the_provider_sections_and_keeps_keychain_keys_out() {
        let directory =
            std::env::temp_dir().join(format!("glyphst-settings-save-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let path = directory.join("glyphst").join("config.kdl");
        let deepl = ProviderSecretId::TranslationApiKey(TranslationProvider::DeepL);
        let google = ProviderSecretId::TranslationApiKey(TranslationProvider::Google);

        let mut loaded = parse_settings(
            r#"
            translation {
                provider "deepl"
                providers {
                    deepl { api_key "in-config" }
                }
            }
            "#,
        )
        .unwrap();
        let config_owned = config_owned_secret_ids(&loaded);
        save_user_settings_to(&path, &loaded, &config_owned).unwrap();

        // The GPUI Reader changes another section meanwhile.
        let mut on_disk = load_settings(&path).unwrap();
        on_disk.keymap_preset = KeymapPreset::Vim;
        write_kdl(&path, on_disk.to_kdl_string()).unwrap();

        loaded.translation.active_provider = TranslationProvider::Google;
        loaded.set_provider_secret(deepl, "edited-in-config");
        loaded.set_provider_secret(google, "for-the-keychain");
        save_user_settings_to(&path, &loaded, &config_owned).unwrap();

        let saved = load_settings(&path).unwrap();
        assert_eq!(saved.keymap_preset, KeymapPreset::Vim, "kept");
        assert_eq!(
            saved.translation.active_provider,
            TranslationProvider::Google
        );
        assert_eq!(saved.provider_secret(deepl), Some("edited-in-config"));
        assert!(saved.provider_secret(google).is_none_or(str::is_empty));
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("for-the-keychain"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        fs::remove_dir_all(&directory).unwrap();
    }
}
