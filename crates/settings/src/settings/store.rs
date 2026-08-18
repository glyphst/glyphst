use super::persistence::*;
use super::provider_secrets::{
    CredentialReadResult, ProviderCredentialBackend, memory_backend, platform_backend,
};
use super::*;
use futures::{
    StreamExt as _,
    channel::{mpsc, oneshot},
};
use gpui::AppContext as _;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

const PROVIDER_SECRET_SAVE_DEBOUNCE: Duration = Duration::from_millis(300);

enum CredentialAction {
    WriteKeychain(String),
    WriteConfig {
        path: PathBuf,
        contents: String,
    },
    Clear {
        config_write: Option<(PathBuf, String)>,
    },
}

struct CredentialCommand {
    id: ProviderSecretId,
    generation: u64,
    intended: ProviderSecretPersistence,
    action: CredentialAction,
    completion: Option<oneshot::Sender<std::result::Result<(), String>>>,
}

struct CredentialActionOutcome {
    result: Result<()>,
    config_secret_cleared: bool,
}

pub fn setup_ui_font(window: &mut Window, cx: &mut App) -> Font {
    let font_settings = cx.global::<SettingsStore>().settings.font.clone();
    window.set_rem_size(gpui::px(font_settings.ui_font_size));
    font_settings.ui_font()
}

pub struct SettingsStore {
    pub settings: Settings,
    pub theme_catalog: ThemeCatalog,
    pub resolved_theme: Theme,
    pub system_appearance: ThemeAppearance,
    pub user_managed: bool,
    pub config_path: PathBuf,
    pub state_path: PathBuf,
    config_loaded_successfully: bool,
    loaded_from_state: bool,
    stale_state_present: bool,
    legacy_state_secrets: BTreeMap<ProviderSecretId, String>,
    provider_secrets_initialized: bool,
    config_owned_secrets: BTreeSet<ProviderSecretId>,
    provider_secret_statuses: BTreeMap<ProviderSecretId, ProviderSecretStatus>,
    provider_secret_generations: BTreeMap<ProviderSecretId, u64>,
    credential_backend: Arc<dyn ProviderCredentialBackend>,
    credential_tx: Option<mpsc::UnboundedSender<CredentialCommand>>,
    credential_warning: Option<String>,
}

impl Global for SettingsStore {}

impl SettingsStore {
    pub fn memory() -> Self {
        let settings = Settings::default();
        let theme_catalog = ThemeCatalog::bundled();
        let system_appearance = ThemeAppearance::Light;
        let resolved_theme =
            theme_catalog.resolve(settings.theme.name(system_appearance), settings.overrides);
        let provider_secret_statuses = settings
            .provider_secret_ids()
            .into_iter()
            .map(|id| (id, ProviderSecretStatus::Missing))
            .collect();
        Self {
            settings,
            theme_catalog,
            resolved_theme,
            system_appearance,
            user_managed: false,
            config_path: PathBuf::new(),
            state_path: PathBuf::new(),
            config_loaded_successfully: false,
            loaded_from_state: false,
            stale_state_present: false,
            legacy_state_secrets: BTreeMap::new(),
            provider_secrets_initialized: true,
            config_owned_secrets: BTreeSet::new(),
            provider_secret_statuses,
            provider_secret_generations: BTreeMap::new(),
            credential_backend: memory_backend(),
            credential_tx: None,
            credential_warning: None,
        }
    }

    pub fn load() -> Self {
        let config_path = config_path();
        let state_path = state_path();
        let themes_path = user_themes_dir();
        if let Err(error) = fs::create_dir_all(&themes_path) {
            eprintln!(
                "failed to create user theme directory {}: {error}",
                themes_path.display()
            );
        }
        let theme_catalog = ThemeCatalog::load(&themes_path);
        let user_managed = config_path.is_file();
        let state_exists = state_path.is_file();
        let mut config_loaded_successfully = false;
        let mut loaded_from_state = false;
        let mut settings = if user_managed {
            match load_settings(&config_path) {
                Ok(settings) => {
                    config_loaded_successfully = true;
                    settings
                }
                Err(error) => {
                    eprintln!(
                        "failed to load settings from {}: {error:#}",
                        config_path.display()
                    );
                    Settings::default()
                }
            }
        } else if state_exists {
            match load_settings(&state_path) {
                Ok(settings) => {
                    loaded_from_state = true;
                    settings
                }
                Err(error) => {
                    eprintln!(
                        "failed to load legacy settings from {}: {error:#}",
                        state_path.display()
                    );
                    Settings::default()
                }
            }
        } else {
            Settings::default()
        };
        validate_theme_selection(&mut settings, &theme_catalog);
        let config_owned_secrets = if config_loaded_successfully {
            settings
                .non_empty_provider_secrets()
                .into_iter()
                .map(|(id, _)| id)
                .collect::<BTreeSet<_>>()
        } else {
            BTreeSet::new()
        };
        let mut stale_state_present = false;
        let mut legacy_state_secrets = if loaded_from_state {
            settings
                .non_empty_provider_secrets()
                .into_iter()
                .collect::<BTreeMap<_, _>>()
        } else {
            BTreeMap::new()
        };
        let mut credential_warning = None;
        if config_loaded_successfully && state_exists {
            match load_settings(&state_path) {
                Ok(state_settings) => {
                    stale_state_present = true;
                    legacy_state_secrets.extend(merge_legacy_state_secrets(
                        &mut settings,
                        &config_owned_secrets,
                        &state_settings,
                    ));
                }
                Err(error) => {
                    let message = format!(
                        "failed to inspect legacy settings at {}: {error:#}",
                        state_path.display()
                    );
                    eprintln!("{message}");
                    credential_warning = Some(message);
                }
            }
        }
        let system_appearance = ThemeAppearance::Light;
        let resolved_theme =
            theme_catalog.resolve(settings.theme.name(system_appearance), settings.overrides);
        let provider_secret_statuses = settings
            .provider_secret_ids()
            .into_iter()
            .map(|id| {
                let status = if config_owned_secrets.contains(&id) {
                    ProviderSecretStatus::Config
                } else {
                    ProviderSecretStatus::Loading
                };
                (id, status)
            })
            .collect();

        Self {
            settings,
            theme_catalog,
            resolved_theme,
            system_appearance,
            user_managed,
            config_path,
            state_path,
            config_loaded_successfully,
            loaded_from_state,
            stale_state_present,
            legacy_state_secrets,
            provider_secrets_initialized: false,
            config_owned_secrets,
            provider_secret_statuses,
            provider_secret_generations: BTreeMap::new(),
            credential_backend: platform_backend(),
            credential_tx: None,
            credential_warning,
        }
    }

    pub fn install_theme_catalog(&self, cx: &mut App) -> Result<()> {
        self.theme_catalog.install_into_component_registry(cx)
    }

    pub fn set_settings(&mut self, settings: Settings) -> Result<()> {
        let mut settings = settings;
        settings.sanitize();
        validate_theme_selection(&mut settings, &self.theme_catalog);
        let resolved_theme = self.theme_catalog.resolve(
            settings.theme.name(self.system_appearance),
            settings.overrides,
        );
        let contents = self.export_settings(&settings);
        write_kdl(&self.config_path, contents)?;
        self.settings = settings;
        self.resolved_theme = resolved_theme;
        self.user_managed = true;
        self.config_loaded_successfully = true;
        self.reconcile_provider_secret_statuses();
        Ok(())
    }

    pub fn apply_imported_settings(mut settings: Settings, cx: &mut App) -> Result<()> {
        Self::ensure_credential_worker(cx);
        settings.sanitize();

        let (imported_secrets, removed_secret_ids) = {
            let store = cx.global_mut::<Self>();
            let current_settings = store.settings.clone();
            let current_secret_ids = current_settings
                .provider_secret_ids()
                .into_iter()
                .collect::<BTreeSet<_>>();
            let mut imported_secrets = Vec::new();

            for id in settings.provider_secret_ids() {
                let imported_secret = settings
                    .provider_secret(id)
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string();
                if imported_secret.is_empty() {
                    if let Some(current_secret) = current_settings
                        .provider_secret(id)
                        .filter(|secret| !secret.trim().is_empty())
                    {
                        settings.set_provider_secret(id, current_secret.to_string());
                    }
                } else {
                    settings.set_provider_secret(id, imported_secret.clone());
                    imported_secrets.push((id, imported_secret));
                }
            }

            let imported_secret_ids = settings
                .provider_secret_ids()
                .into_iter()
                .collect::<BTreeSet<_>>();
            let removed_secret_ids = current_secret_ids
                .difference(&imported_secret_ids)
                .copied()
                .collect::<Vec<_>>();
            let previous_config_owned_secrets = store.config_owned_secrets.clone();
            for (id, _) in &imported_secrets {
                store.config_owned_secrets.remove(id);
            }
            for id in &removed_secret_ids {
                store.config_owned_secrets.remove(id);
            }

            if let Err(error) = store.set_settings(settings) {
                store.config_owned_secrets = previous_config_owned_secrets;
                return Err(error);
            }

            (imported_secrets, removed_secret_ids)
        };

        for id in removed_secret_ids {
            Self::delete_provider_secret_credential(id, cx);
        }
        for (id, secret) in imported_secrets {
            Self::update_provider_secret(id, secret, cx);
        }

        Ok(())
    }

    pub fn reset_all_settings(cx: &mut App) -> Result<()> {
        Self::ensure_credential_worker(cx);
        let default_secret_ids = Settings::default()
            .provider_secret_ids()
            .into_iter()
            .collect::<BTreeSet<_>>();
        let secret_ids = {
            let store = cx.global_mut::<Self>();
            let secret_ids = store
                .settings
                .provider_secret_ids()
                .into_iter()
                .chain(default_secret_ids.iter().copied())
                .collect::<BTreeSet<_>>();
            let previous_config_owned_secrets = store.config_owned_secrets.clone();
            for id in &secret_ids {
                store.config_owned_secrets.remove(id);
            }

            if let Err(error) = store.set_settings(Settings::default()) {
                store.config_owned_secrets = previous_config_owned_secrets;
                return Err(error);
            }

            secret_ids
        };

        for id in secret_ids {
            Self::delete_provider_secret_credential(id, cx);
        }

        Ok(())
    }

    pub fn set_settings_in_memory(&mut self, settings: Settings) {
        let mut settings = settings;
        settings.sanitize();
        validate_theme_selection(&mut settings, &self.theme_catalog);
        let resolved_theme = self.theme_catalog.resolve(
            settings.theme.name(self.system_appearance),
            settings.overrides,
        );
        self.settings = settings;
        self.resolved_theme = resolved_theme;
        self.reconcile_provider_secret_statuses();
    }

    pub fn set_system_appearance(&mut self, appearance: ThemeAppearance) {
        if self.system_appearance == appearance {
            return;
        }
        self.system_appearance = appearance;
        self.resolved_theme = self.theme_catalog.resolve(
            self.settings.theme.name(appearance),
            self.settings.overrides,
        );
    }

    pub fn save(&mut self) -> Result<()> {
        if !self.user_managed
            && self.settings.without_provider_secrets()
                == Settings::default().without_provider_secrets()
        {
            return Ok(());
        }
        let (path, contents) = self.persistence_snapshot();
        Self::write_snapshot(&path, contents)?;
        self.settings_snapshot_written(&path);
        Ok(())
    }

    pub fn persistence_snapshot(&self) -> (PathBuf, String) {
        (self.config_path.clone(), self.export())
    }

    pub fn write_snapshot(path: &Path, contents: String) -> Result<()> {
        write_kdl(path, contents)
    }

    pub fn settings_snapshot_written(&mut self, path: &Path) {
        if path == self.config_path {
            self.user_managed = true;
            self.config_loaded_successfully = true;
        }
    }

    pub fn override_settings(&mut self, settings: Settings) {
        self.settings = settings;
        self.settings.sanitize();
        validate_theme_selection(&mut self.settings, &self.theme_catalog);
        self.resolved_theme = self.theme_catalog.resolve(
            self.settings.theme.name(self.system_appearance),
            self.settings.overrides,
        );
        self.reconcile_provider_secret_statuses();
    }

    pub fn export(&self) -> String {
        self.export_settings(&self.settings)
    }

    pub fn export_to(&self, path: &Path) -> Result<()> {
        write_kdl(path, self.export())
    }

    fn export_settings(&self, settings: &Settings) -> String {
        let mut settings = settings.clone();
        for id in settings.provider_secret_ids() {
            if !self.config_owned_secrets.contains(&id) {
                settings.set_provider_secret(id, String::new());
            }
        }
        settings.to_kdl_string()
    }

    fn reconcile_provider_secret_statuses(&mut self) {
        let ids = self
            .settings
            .provider_secret_ids()
            .into_iter()
            .collect::<BTreeSet<_>>();
        self.provider_secret_statuses
            .retain(|id, _| ids.contains(id));
        self.provider_secret_generations
            .retain(|id, _| ids.contains(id));
        self.config_owned_secrets.retain(|id| ids.contains(id));
        for id in ids {
            self.provider_secret_statuses.entry(id).or_insert_with(|| {
                if self.config_owned_secrets.contains(&id) {
                    ProviderSecretStatus::Config
                } else if self.provider_secrets_initialized {
                    ProviderSecretStatus::Missing
                } else {
                    ProviderSecretStatus::Loading
                }
            });
        }
    }

    pub fn provider_secret_status(&self, id: ProviderSecretId) -> ProviderSecretStatus {
        self.provider_secret_statuses
            .get(&id)
            .cloned()
            .unwrap_or(ProviderSecretStatus::Missing)
    }

    pub fn provider_secret_is_config_owned(&self, id: ProviderSecretId) -> bool {
        self.config_owned_secrets.contains(&id)
    }

    pub fn provider_secret_is_loading(&self, id: ProviderSecretId) -> bool {
        self.provider_secret_status(id).is_loading()
            && self.settings.provider_secret(id).is_none_or(str::is_empty)
    }

    pub fn ai_provider_credentials_loading(&self, provider: AiProvider) -> bool {
        provider != AiProvider::ChatGptSubscription
            && self.provider_secret_is_loading(ProviderSecretId::AiApiKey(provider))
    }

    pub fn translation_credentials_loading(&self) -> bool {
        match self.settings.translation.active_provider {
            TranslationProvider::Ai => self
                .ai_provider_credentials_loading(self.settings.ai.resolved_translation().provider),
            provider => {
                self.provider_secret_is_loading(ProviderSecretId::TranslationApiKey(provider))
            }
        }
    }

    pub fn document_translation_credentials_loading(&self) -> bool {
        if self.translation_credentials_loading() {
            return true;
        }

        let ocr_loading = match (self.settings.ocr.mode, self.settings.ocr.provider) {
            (OcrMode::Off, _) => false,
            (_, OcrProvider::Paddle) => {
                self.provider_secret_is_loading(ProviderSecretId::PaddleOcrToken)
            }
            (_, OcrProvider::Unlimited) => {
                self.provider_secret_is_loading(ProviderSecretId::UnlimitedOcrApiKey)
                    || self.provider_secret_is_loading(ProviderSecretId::UnlimitedOcrSecretKey)
            }
        };
        if ocr_loading {
            return true;
        }

        self.settings.translation.content_review.mode == PdfContentReviewMode::Ambiguous
            && self
                .settings
                .translation
                .content_review
                .model
                .as_ref()
                .and_then(|model| self.settings.ai.resolved_model(model))
                .is_some_and(|resolved| self.ai_provider_credentials_loading(resolved.provider))
    }

    pub fn credential_warning(&self) -> Option<&str> {
        self.credential_warning.as_deref()
    }

    pub fn initialize_provider_secrets(cx: &mut App) {
        Self::ensure_credential_worker(cx);
        let (
            backend,
            credential_tx,
            read_requests,
            migration_requests,
            loaded_from_state,
            stale_state_present,
        ) = {
            let store = cx.global_mut::<Self>();
            if store.provider_secrets_initialized {
                return;
            }
            store.provider_secrets_initialized = true;
            let legacy_secrets = store.legacy_state_secrets.clone();
            let mut read_requests = Vec::new();
            let mut migration_requests = Vec::new();
            for id in store.settings.provider_secret_ids() {
                if store.config_owned_secrets.contains(&id) {
                    store
                        .provider_secret_statuses
                        .insert(id, ProviderSecretStatus::Config);
                    continue;
                }
                let generation = *store.provider_secret_generations.get(&id).unwrap_or(&0);
                if let Some(secret) = legacy_secrets.get(&id) {
                    migration_requests.push((id, generation, secret.clone()));
                } else {
                    store
                        .provider_secret_statuses
                        .insert(id, ProviderSecretStatus::Loading);
                    read_requests.push((id, generation));
                }
            }
            (
                store.credential_backend.clone(),
                store
                    .credential_tx
                    .as_ref()
                    .expect("credential worker was initialized")
                    .clone(),
                read_requests,
                migration_requests,
                store.loaded_from_state,
                store.stale_state_present,
            )
        };

        let reads = read_requests
            .into_iter()
            .map(|(id, generation)| {
                let task = backend.read(&id.credential_url(), cx);
                (id, generation, task)
            })
            .collect::<Vec<_>>();
        let migration_completions = migration_requests
            .into_iter()
            .map(|(id, generation, secret)| {
                let (completion, receiver) = oneshot::channel();
                let send_result = credential_tx.unbounded_send(CredentialCommand {
                    id,
                    generation,
                    intended: ProviderSecretPersistence::Keychain,
                    action: CredentialAction::WriteKeychain(secret),
                    completion: Some(completion),
                });
                (id, send_result, receiver)
            })
            .collect::<Vec<_>>();

        cx.spawn(async move |cx| {
            for (id, generation, task) in reads {
                let result = task.await;
                cx.update(|cx| Self::finish_provider_secret_load(id, generation, result, cx));
            }

            let mut migration_failed = false;
            for (id, send_result, completion) in migration_completions {
                let result = match send_result {
                    Ok(()) => completion.await.unwrap_or_else(|_| {
                        Err("credential worker stopped before migration completed".to_string())
                    }),
                    Err(_) => Err("credential worker is unavailable".to_string()),
                };
                if let Err(error) = result {
                    migration_failed = true;
                    cx.update(|cx| {
                        let store = cx.global_mut::<Self>();
                        store.credential_warning = Some(format!(
                            "failed to migrate {id:?} to the system keychain: {error}"
                        ));
                        cx.refresh_windows();
                    });
                }
            }

            if (loaded_from_state || stale_state_present) && !migration_failed {
                let (config_migration, state_path) = cx.update(|cx| {
                    let store = cx.global::<Self>();
                    let config_migration = loaded_from_state.then(|| {
                        let settings = store.settings.without_provider_secrets();
                        let defaults = Settings::default().without_provider_secrets();
                        (
                            store.config_path.clone(),
                            settings.to_kdl_string(),
                            settings != defaults,
                        )
                    });
                    (config_migration, store.state_path.clone())
                });
                let config_result =
                    if let Some((config_path, contents, needs_config)) = config_migration {
                        if needs_config {
                            cx.background_spawn(async move {
                                write_kdl(&config_path, contents)
                                    .context("failed to write migrated user settings")
                            })
                            .await
                        } else {
                            Ok(())
                        }
                    } else {
                        Ok(())
                    };
                let migration_result = match config_result {
                    Ok(()) => remove_legacy_state_file(&state_path),
                    Err(error) => Err(error),
                };
                cx.update(|cx| {
                    let store = cx.global_mut::<Self>();
                    match migration_result {
                        Ok(()) => {
                            store.loaded_from_state = false;
                            store.stale_state_present = false;
                            store.legacy_state_secrets.clear();
                            let config_exists = store.config_path.is_file();
                            store.user_managed = config_exists;
                            store.config_loaded_successfully = config_exists;
                        }
                        Err(error) => {
                            store.credential_warning = Some(format!(
                                "failed to finish legacy settings migration: {error:#}"
                            ));
                        }
                    }
                    cx.refresh_windows();
                });
            }
        })
        .detach();
    }

    fn finish_provider_secret_load(
        id: ProviderSecretId,
        generation: u64,
        result: CredentialReadResult,
        cx: &mut App,
    ) {
        let store = cx.global_mut::<Self>();
        if store
            .provider_secret_generations
            .get(&id)
            .copied()
            .unwrap_or(0)
            != generation
            || store.config_owned_secrets.contains(&id)
        {
            return;
        }
        match result {
            Ok(Some((_, bytes))) => match String::from_utf8(bytes) {
                Ok(secret) if !secret.trim().is_empty() => {
                    store.settings.set_provider_secret(id, secret);
                    store
                        .provider_secret_statuses
                        .insert(id, ProviderSecretStatus::Keychain);
                }
                Ok(_) => {
                    store.settings.set_provider_secret(id, String::new());
                    store
                        .provider_secret_statuses
                        .insert(id, ProviderSecretStatus::Missing);
                }
                Err(_) => {
                    let message = format!("saved credential for {id:?} is not valid UTF-8");
                    store
                        .provider_secret_statuses
                        .insert(id, ProviderSecretStatus::Unavailable(message));
                }
            },
            Ok(None) => {
                store.settings.set_provider_secret(id, String::new());
                store
                    .provider_secret_statuses
                    .insert(id, ProviderSecretStatus::Missing);
            }
            Err(error) => {
                let message = format!("failed to read {id:?} from the system keychain: {error:#}");
                store
                    .provider_secret_statuses
                    .insert(id, ProviderSecretStatus::Unavailable(message));
            }
        }
        cx.refresh_windows();
    }

    pub fn update_provider_secret(id: ProviderSecretId, value: String, cx: &mut App) {
        Self::ensure_credential_worker(cx);
        let value = value.trim().to_string();
        if value.is_empty() {
            Self::clear_provider_secret(id, cx);
            return;
        }
        let (generation, intended, action, sender) = {
            let store = cx.global_mut::<Self>();
            if !store.settings.set_provider_secret(id, value.clone()) {
                return;
            }
            let generation = store
                .provider_secret_generations
                .entry(id)
                .and_modify(|generation| *generation = generation.wrapping_add(1))
                .or_insert(1)
                .to_owned();
            let intended = if store.config_owned_secrets.contains(&id) {
                ProviderSecretPersistence::Config
            } else {
                ProviderSecretPersistence::Keychain
            };
            let action = match intended {
                ProviderSecretPersistence::Config => CredentialAction::WriteConfig {
                    path: store.config_path.clone(),
                    contents: store.export(),
                },
                ProviderSecretPersistence::Keychain => CredentialAction::WriteKeychain(value),
            };
            store
                .provider_secret_statuses
                .insert(id, ProviderSecretStatus::Saving(intended));
            (
                generation,
                intended,
                action,
                store.credential_tx.as_ref().cloned(),
            )
        };
        let Some(sender) = sender else {
            return;
        };
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(PROVIDER_SECRET_SAVE_DEBOUNCE)
                .await;
            let is_current = cx.update(|cx| {
                cx.global::<Self>()
                    .provider_secret_generations
                    .get(&id)
                    .copied()
                    .unwrap_or(0)
                    == generation
            });
            if is_current {
                let _ = sender.unbounded_send(CredentialCommand {
                    id,
                    generation,
                    intended,
                    action,
                    completion: None,
                });
            }
        })
        .detach();
        cx.refresh_windows();
    }

    pub fn clear_provider_secret(id: ProviderSecretId, cx: &mut App) {
        Self::ensure_credential_worker(cx);
        let command = {
            let store = cx.global_mut::<Self>();
            if !store.settings.set_provider_secret(id, String::new()) {
                return;
            }
            let generation = store
                .provider_secret_generations
                .entry(id)
                .and_modify(|generation| *generation = generation.wrapping_add(1))
                .or_insert(1)
                .to_owned();
            let intended = if store.config_owned_secrets.contains(&id) {
                ProviderSecretPersistence::Config
            } else {
                ProviderSecretPersistence::Keychain
            };
            let config_write = store
                .config_owned_secrets
                .contains(&id)
                .then(|| (store.config_path.clone(), store.export()));
            store
                .provider_secret_statuses
                .insert(id, ProviderSecretStatus::Saving(intended));
            store.credential_tx.as_ref().cloned().map(|sender| {
                (
                    sender,
                    CredentialCommand {
                        id,
                        generation,
                        intended,
                        action: CredentialAction::Clear { config_write },
                        completion: None,
                    },
                )
            })
        };
        if let Some((sender, command)) = command {
            let _ = sender.unbounded_send(command);
        }
        cx.refresh_windows();
    }

    pub fn delete_provider_secret_credential(id: ProviderSecretId, cx: &mut App) {
        Self::ensure_credential_worker(cx);
        let command = {
            let store = cx.global_mut::<Self>();
            let generation = store
                .provider_secret_generations
                .entry(id)
                .and_modify(|generation| *generation = generation.wrapping_add(1))
                .or_insert(1)
                .to_owned();
            store.credential_tx.as_ref().cloned().map(|sender| {
                (
                    sender,
                    CredentialCommand {
                        id,
                        generation,
                        intended: ProviderSecretPersistence::Keychain,
                        action: CredentialAction::Clear { config_write: None },
                        completion: None,
                    },
                )
            })
        };
        if let Some((sender, command)) = command {
            let _ = sender.unbounded_send(command);
        }
    }

    fn ensure_credential_worker(cx: &mut App) {
        if cx.global::<Self>().credential_tx.is_some() {
            return;
        }
        let (tx, mut rx) = mpsc::unbounded::<CredentialCommand>();
        cx.global_mut::<Self>().credential_tx = Some(tx);
        cx.spawn(async move |cx| {
            while let Some(mut command) = rx.next().await {
                let backend = cx.update(|cx| cx.global::<Self>().credential_backend.clone());
                let outcome = match &command.action {
                    CredentialAction::WriteKeychain(secret) => {
                        let task = cx.update(|cx| {
                            backend.write(
                                &command.id.credential_url(),
                                command.id.credential_username(),
                                secret.as_bytes(),
                                cx,
                            )
                        });
                        CredentialActionOutcome {
                            result: task.await.context("failed to save provider secret"),
                            config_secret_cleared: false,
                        }
                    }
                    CredentialAction::WriteConfig { path, contents } => {
                        let path = path.clone();
                        let contents = contents.clone();
                        CredentialActionOutcome {
                            result: cx
                                .background_spawn(async move { write_kdl(&path, contents) })
                                .await
                                .context("failed to save provider secret to config"),
                            config_secret_cleared: false,
                        }
                    }
                    CredentialAction::Clear { config_write } => {
                        let url = command.id.credential_url();
                        let read = cx.update(|cx| backend.read(&url, cx)).await;
                        let keychain_result = match read {
                            Ok(Some(_)) => cx
                                .update(|cx| backend.delete(&url, cx))
                                .await
                                .context("failed to delete provider secret"),
                            Ok(None) => Ok(()),
                            Err(error) => Err(error).context("failed to inspect provider secret"),
                        };
                        let config_result = if let Some((path, contents)) = config_write {
                            let path = path.clone();
                            let contents = contents.clone();
                            cx.background_spawn(async move { write_kdl(&path, contents) })
                                .await
                                .context("failed to remove provider secret from config")
                        } else {
                            Ok(())
                        };
                        let config_secret_cleared = config_write.is_some() && config_result.is_ok();
                        let result = match (keychain_result, config_result) {
                            (Ok(()), Ok(())) => Ok(()),
                            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
                            (Err(keychain_error), Err(config_error)) => {
                                Err(anyhow::anyhow!("{keychain_error:#}; {config_error:#}"))
                            }
                        };
                        CredentialActionOutcome {
                            result,
                            config_secret_cleared,
                        }
                    }
                };
                let completion_result = outcome
                    .result
                    .as_ref()
                    .map(|_| ())
                    .map_err(|error| format!("{error:#}"));
                let completion = command.completion.take();
                cx.update(|cx| {
                    let store = cx.global_mut::<Self>();
                    if store
                        .provider_secret_generations
                        .get(&command.id)
                        .copied()
                        .unwrap_or(0)
                        != command.generation
                    {
                        if let Err(error) = outcome.result {
                            store.credential_warning = Some(format!("{error:#}"));
                            cx.refresh_windows();
                        }
                        return;
                    }
                    if !store.settings.provider_secret_ids().contains(&command.id) {
                        if let Err(error) = outcome.result {
                            store.credential_warning = Some(format!("{error:#}"));
                            cx.refresh_windows();
                        }
                        return;
                    }
                    if outcome.config_secret_cleared {
                        store.config_owned_secrets.remove(&command.id);
                    }
                    match outcome.result {
                        Ok(()) => match command.action {
                            CredentialAction::WriteKeychain(_) => {
                                store
                                    .provider_secret_statuses
                                    .insert(command.id, ProviderSecretStatus::Keychain);
                            }
                            CredentialAction::WriteConfig { .. } => {
                                store.user_managed = true;
                                store.config_loaded_successfully = true;
                                store
                                    .provider_secret_statuses
                                    .insert(command.id, ProviderSecretStatus::Config);
                            }
                            CredentialAction::Clear { config_write } => {
                                if config_write.is_some() {
                                    store.user_managed = true;
                                    store.config_loaded_successfully = true;
                                }
                                store.config_owned_secrets.remove(&command.id);
                                store
                                    .provider_secret_statuses
                                    .insert(command.id, ProviderSecretStatus::Missing);
                            }
                        },
                        Err(error) => {
                            let message = format!("{error:#}");
                            let status = if matches!(command.action, CredentialAction::Clear { .. })
                                && (outcome.config_secret_cleared
                                    || command.intended == ProviderSecretPersistence::Keychain)
                            {
                                ProviderSecretStatus::Unavailable(message.clone())
                            } else {
                                ProviderSecretStatus::SessionOnly {
                                    intended: command.intended,
                                    error: message.clone(),
                                }
                            };
                            store.provider_secret_statuses.insert(command.id, status);
                            store.credential_warning = Some(message);
                        }
                    }
                    cx.refresh_windows();
                });
                if let Some(completion) = completion {
                    let _ = completion.send(completion_result);
                }
            }
        })
        .detach();
    }
}

fn remove_legacy_state_file(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };
    anyhow::ensure!(
        metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
        "refusing to remove non-regular legacy settings path {}",
        path.display()
    );
    fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
}

fn merge_legacy_state_secrets(
    settings: &mut Settings,
    config_owned_secrets: &BTreeSet<ProviderSecretId>,
    legacy_settings: &Settings,
) -> BTreeMap<ProviderSecretId, String> {
    let mut migrated = BTreeMap::new();
    for id in settings.provider_secret_ids() {
        if config_owned_secrets.contains(&id) {
            continue;
        }
        let Some(secret) = legacy_settings
            .provider_secret(id)
            .filter(|secret| !secret.is_empty())
        else {
            continue;
        };
        settings.set_provider_secret(id, secret.to_string());
        migrated.insert(id, secret.to_string());
    }
    migrated
}

/// Keep gpui-components and Glyphst's document/workspace adapter on the same
/// selected theme. Call this after a theme, appearance, override, or UI-font
/// setting changes.
pub fn sync_component_theme(cx: &mut App) {
    let (light, dark, active, overrides, font) = {
        let store = cx.global::<SettingsStore>();
        let light = store
            .theme_catalog
            .config(&store.settings.theme.light)
            .or_else(|| store.theme_catalog.config(DEFAULT_LIGHT_THEME_ID))
            .expect("bundled default light theme is missing");
        let dark = store
            .theme_catalog
            .config(&store.settings.theme.dark)
            .or_else(|| store.theme_catalog.config(DEFAULT_DARK_THEME_ID))
            .expect("bundled default dark theme is missing");
        let active = match store
            .settings
            .theme
            .effective_appearance(store.system_appearance)
        {
            ThemeAppearance::Light => light.clone(),
            ThemeAppearance::Dark => dark.clone(),
        };
        (
            light,
            dark,
            active,
            store.settings.overrides,
            store.settings.font.clone(),
        )
    };

    let theme = gpui_component::Theme::global_mut(cx);
    theme.light_theme = light;
    theme.dark_theme = dark;
    theme.apply_config(&active);
    apply_component_overrides(theme, overrides);
    theme.font_family = font.ui_font_family.into();
    theme.font_size = gpui::px(font.ui_font_size);
    cx.refresh_windows();
}

fn validate_theme_selection(settings: &mut Settings, catalog: &ThemeCatalog) {
    if !catalog.contains_for_appearance(&settings.theme.light, ThemeAppearance::Light) {
        settings.theme.light = DEFAULT_LIGHT_THEME_ID.to_string();
    }
    if !catalog.contains_for_appearance(&settings.theme.dark, ThemeAppearance::Dark) {
        settings.theme.dark = DEFAULT_DARK_THEME_ID.to_string();
    }
}

#[cfg(test)]
mod credential_tests {
    use super::*;
    use gpui::{App, Task, TestAppContext};
    use std::{
        collections::HashMap,
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
    };

    static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[derive(Default)]
    struct TestCredentialBackend {
        entries: Mutex<HashMap<String, (String, Vec<u8>)>>,
        reads: Mutex<Vec<String>>,
        writes: Mutex<Vec<String>>,
        deletes: Mutex<Vec<String>>,
        fail_reads: AtomicBool,
        fail_writes: AtomicBool,
        fail_deletes: AtomicBool,
    }

    impl TestCredentialBackend {
        fn seed(&self, id: ProviderSecretId, secret: &str) {
            self.entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(
                    id.credential_url(),
                    (
                        id.credential_username().to_string(),
                        secret.as_bytes().to_vec(),
                    ),
                );
        }

        fn secret(&self, id: ProviderSecretId) -> Option<String> {
            self.entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&id.credential_url())
                .and_then(|(_, secret)| String::from_utf8(secret.clone()).ok())
        }

        fn read_urls(&self) -> Vec<String> {
            self.reads
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

        fn write_urls(&self) -> Vec<String> {
            self.writes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    impl ProviderCredentialBackend for TestCredentialBackend {
        fn read(&self, url: &str, _: &App) -> Task<CredentialReadResult> {
            self.reads
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(url.to_string());
            if self.fail_reads.load(Ordering::Relaxed) {
                return Task::ready(Err(anyhow::anyhow!("simulated credential read failure")));
            }
            let entry = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(url)
                .cloned();
            Task::ready(Ok(entry))
        }

        fn write(&self, url: &str, username: &str, secret: &[u8], _: &App) -> Task<Result<()>> {
            self.writes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(url.to_string());
            if self.fail_writes.load(Ordering::Relaxed) {
                return Task::ready(Err(anyhow::anyhow!("simulated credential write failure")));
            }
            self.entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(url.to_string(), (username.to_string(), secret.to_vec()));
            Task::ready(Ok(()))
        }

        fn delete(&self, url: &str, _: &App) -> Task<Result<()>> {
            self.deletes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(url.to_string());
            if self.fail_deletes.load(Ordering::Relaxed) {
                return Task::ready(Err(anyhow::anyhow!("simulated credential delete failure")));
            }
            self.entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(url);
            Task::ready(Ok(()))
        }
    }

    #[derive(Clone, Copy)]
    enum TestSettingsSource {
        Empty,
        Config,
        LegacyState,
    }

    fn test_root(name: &str) -> PathBuf {
        let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "glyphst-provider-credentials-{name}-{}-{sequence}",
            std::process::id()
        ))
    }

    fn test_store(
        settings: Settings,
        root: &Path,
        source: TestSettingsSource,
        backend: Arc<dyn ProviderCredentialBackend>,
    ) -> SettingsStore {
        let mut store = SettingsStore::memory();
        store.config_path = root.join("config").join("config.kdl");
        store.state_path = root.join("state").join("config.kdl");
        store.set_settings_in_memory(settings);
        store.credential_backend = backend;
        store.credential_tx = None;
        store.provider_secrets_initialized = false;
        store.credential_warning = None;
        store.provider_secret_generations.clear();
        store.config_owned_secrets = match source {
            TestSettingsSource::Config => store
                .settings
                .non_empty_provider_secrets()
                .into_iter()
                .map(|(id, _)| id)
                .collect(),
            TestSettingsSource::Empty | TestSettingsSource::LegacyState => BTreeSet::new(),
        };
        store.provider_secret_statuses = store
            .settings
            .provider_secret_ids()
            .into_iter()
            .map(|id| {
                let status = if store.config_owned_secrets.contains(&id) {
                    ProviderSecretStatus::Config
                } else {
                    ProviderSecretStatus::Loading
                };
                (id, status)
            })
            .collect();
        store.user_managed = matches!(source, TestSettingsSource::Config);
        store.config_loaded_successfully = matches!(source, TestSettingsSource::Config);
        store.loaded_from_state = matches!(source, TestSettingsSource::LegacyState);
        store.stale_state_present = false;
        store.legacy_state_secrets = if store.loaded_from_state {
            store
                .settings
                .non_empty_provider_secrets()
                .into_iter()
                .collect()
        } else {
            BTreeMap::new()
        };
        store
    }

    fn install_store(cx: &mut TestAppContext, store: SettingsStore) {
        cx.update(|cx| cx.set_global(store));
    }

    fn initialize_credentials(cx: &mut TestAppContext) {
        cx.update(SettingsStore::initialize_provider_secrets);
        cx.run_until_parked();
    }

    fn finish_debounced_write(cx: &mut TestAppContext) {
        cx.run_until_parked();
        cx.executor().advance_clock(PROVIDER_SECRET_SAVE_DEBOUNCE);
        cx.run_until_parked();
    }

    #[test]
    fn credential_ids_are_provider_bound_and_stable() {
        assert_eq!(
            ProviderSecretId::AiApiKey(AiProvider::OpenAi).credential_url(),
            "https://glyphst.app/credentials/providers/v1/ai/openai/api-key"
        );
        assert_eq!(
            ProviderSecretId::TranslationApiKey(TranslationProvider::DeepL).credential_url(),
            "https://glyphst.app/credentials/providers/v1/translation/deepl/api-key"
        );
        assert_eq!(
            ProviderSecretId::PaddleOcrToken.credential_url(),
            "https://glyphst.app/credentials/providers/v1/ocr/paddle/token"
        );
    }

    #[test]
    fn legacy_state_overlay_respects_config_owned_fields() {
        let config_id = ProviderSecretId::AiApiKey(AiProvider::OpenAi);
        let legacy_id = ProviderSecretId::TranslationApiKey(TranslationProvider::DeepL);
        let mut settings = Settings::default();
        settings.set_provider_secret(config_id, "configured-openai-key");
        let mut legacy = Settings::default();
        legacy.set_provider_secret(config_id, "obsolete-openai-key");
        legacy.set_provider_secret(legacy_id, "unmigrated-deepl-key");

        let migrated =
            merge_legacy_state_secrets(&mut settings, &BTreeSet::from([config_id]), &legacy);

        assert_eq!(
            settings.provider_secret(config_id),
            Some("configured-openai-key")
        );
        assert_eq!(
            settings.provider_secret(legacy_id),
            Some("unmigrated-deepl-key")
        );
        assert_eq!(migrated.len(), 1);
        assert_eq!(
            migrated.get(&legacy_id).map(String::as_str),
            Some("unmigrated-deepl-key")
        );
    }

    #[test]
    fn provider_actions_wait_only_for_selected_empty_credentials() {
        let mut store = SettingsStore::memory();
        let ai_id = ProviderSecretId::AiApiKey(store.settings.ai.resolved_translation().provider);
        store
            .provider_secret_statuses
            .insert(ai_id, ProviderSecretStatus::Loading);
        assert!(store.translation_credentials_loading());

        store
            .settings
            .set_provider_secret(ai_id, "already-available-key");
        assert!(!store.translation_credentials_loading());

        store.settings.set_provider_secret(ai_id, String::new());
        store
            .provider_secret_statuses
            .insert(ai_id, ProviderSecretStatus::Missing);
        store.provider_secret_statuses.insert(
            ProviderSecretId::UnlimitedOcrApiKey,
            ProviderSecretStatus::Loading,
        );
        assert!(!store.translation_credentials_loading());
        assert!(store.document_translation_credentials_loading());

        store.settings.ocr.mode = OcrMode::Off;
        assert!(!store.document_translation_credentials_loading());
    }

    #[gpui::test]
    fn config_secret_overrides_keychain_per_field(cx: &mut TestAppContext) {
        let root = test_root("config-precedence");
        let backend = Arc::new(TestCredentialBackend::default());
        let config_id = ProviderSecretId::AiApiKey(AiProvider::OpenAi);
        let keychain_id = ProviderSecretId::TranslationApiKey(TranslationProvider::DeepL);
        backend.seed(config_id, "stored-openai-key");
        backend.seed(keychain_id, "stored-deepl-key");
        let mut settings = Settings::default();
        settings.ai.provider_config_mut(AiProvider::OpenAi).api_key =
            "configured-openai-key".to_string();
        write_kdl(
            &root.join("config").join("config.kdl"),
            settings.to_kdl_string(),
        )
        .unwrap();
        install_store(
            cx,
            test_store(settings, &root, TestSettingsSource::Config, backend.clone()),
        );

        initialize_credentials(cx);

        cx.update(|cx| {
            let store = cx.global::<SettingsStore>();
            assert_eq!(
                store.settings.provider_secret(config_id),
                Some("configured-openai-key")
            );
            assert_eq!(
                store.settings.provider_secret(keychain_id),
                Some("stored-deepl-key")
            );
            assert_eq!(
                store.provider_secret_status(config_id),
                ProviderSecretStatus::Config
            );
            assert_eq!(
                store.provider_secret_status(keychain_id),
                ProviderSecretStatus::Keychain
            );
            let exported = store.export();
            assert!(exported.contains("configured-openai-key"));
            assert!(!exported.contains("stored-deepl-key"));
        });
        assert!(!backend.read_urls().contains(&config_id.credential_url()));

        fs::remove_dir_all(root).ok();
    }

    #[gpui::test]
    fn new_secret_uses_keychain_and_never_plaintext_fallback(cx: &mut TestAppContext) {
        let root = test_root("session-only");
        let backend = Arc::new(TestCredentialBackend::default());
        backend.fail_writes.store(true, Ordering::Relaxed);
        let id = ProviderSecretId::AiApiKey(AiProvider::OpenAi);
        let mut store = test_store(
            Settings::default(),
            &root,
            TestSettingsSource::Empty,
            backend,
        );
        store.provider_secrets_initialized = true;
        store
            .provider_secret_statuses
            .insert(id, ProviderSecretStatus::Missing);
        install_store(cx, store);

        cx.update(|cx| {
            SettingsStore::update_provider_secret(id, "session-only-key".to_string(), cx)
        });
        finish_debounced_write(cx);

        cx.update(|cx| {
            let store = cx.global::<SettingsStore>();
            assert_eq!(store.settings.provider_secret(id), Some("session-only-key"));
            assert!(matches!(
                store.provider_secret_status(id),
                ProviderSecretStatus::SessionOnly {
                    intended: ProviderSecretPersistence::Keychain,
                    ..
                }
            ));
            assert!(!store.export().contains("session-only-key"));
            assert!(store.credential_warning().is_some());
        });
        assert!(!root.join("config").join("config.kdl").exists());

        fs::remove_dir_all(root).ok();
    }

    #[gpui::test]
    fn config_owned_secret_edit_updates_the_config(cx: &mut TestAppContext) {
        let root = test_root("config-edit");
        let backend = Arc::new(TestCredentialBackend::default());
        let id = ProviderSecretId::AiApiKey(AiProvider::OpenAi);
        let mut settings = Settings::default();
        settings.ai.provider_config_mut(AiProvider::OpenAi).api_key = "old-config-key".to_string();
        let config_path = root.join("config").join("config.kdl");
        write_kdl(&config_path, settings.to_kdl_string()).unwrap();
        let mut store = test_store(settings, &root, TestSettingsSource::Config, backend.clone());
        store.provider_secrets_initialized = true;
        install_store(cx, store);

        cx.update(|cx| SettingsStore::update_provider_secret(id, "new-config-key".to_string(), cx));
        finish_debounced_write(cx);

        let persisted = parse_settings(&fs::read_to_string(config_path).unwrap()).unwrap();
        assert_eq!(persisted.provider_secret(id), Some("new-config-key"));
        assert!(backend.write_urls().is_empty());
        cx.update(|cx| {
            assert_eq!(
                cx.global::<SettingsStore>().provider_secret_status(id),
                ProviderSecretStatus::Config
            );
        });

        fs::remove_dir_all(root).ok();
    }

    #[gpui::test]
    fn clear_removes_config_and_keychain_copies(cx: &mut TestAppContext) {
        let root = test_root("clear");
        let backend = Arc::new(TestCredentialBackend::default());
        let id = ProviderSecretId::AiApiKey(AiProvider::OpenAi);
        backend.seed(id, "stale-stored-key");
        let mut settings = Settings::default();
        settings.ai.provider_config_mut(AiProvider::OpenAi).api_key = "configured-key".to_string();
        let config_path = root.join("config").join("config.kdl");
        write_kdl(&config_path, settings.to_kdl_string()).unwrap();
        let mut store = test_store(settings, &root, TestSettingsSource::Config, backend.clone());
        store.provider_secrets_initialized = true;
        install_store(cx, store);

        cx.update(|cx| SettingsStore::clear_provider_secret(id, cx));
        cx.run_until_parked();

        assert_eq!(backend.secret(id), None);
        let persisted = parse_settings(&fs::read_to_string(config_path).unwrap()).unwrap();
        assert_eq!(persisted.provider_secret(id), Some(""));
        cx.update(|cx| {
            let store = cx.global::<SettingsStore>();
            assert_eq!(store.settings.provider_secret(id), Some(""));
            assert!(!store.provider_secret_is_config_owned(id));
            assert_eq!(
                store.provider_secret_status(id),
                ProviderSecretStatus::Missing
            );
        });

        fs::remove_dir_all(root).ok();
    }

    #[gpui::test]
    fn clear_still_removes_config_when_keychain_is_unavailable(cx: &mut TestAppContext) {
        let root = test_root("clear-read-failure");
        let backend = Arc::new(TestCredentialBackend::default());
        backend.fail_reads.store(true, Ordering::Relaxed);
        let id = ProviderSecretId::AiApiKey(AiProvider::OpenAi);
        let mut settings = Settings::default();
        settings.ai.provider_config_mut(AiProvider::OpenAi).api_key = "configured-key".to_string();
        let config_path = root.join("config").join("config.kdl");
        write_kdl(&config_path, settings.to_kdl_string()).unwrap();
        let mut store = test_store(settings, &root, TestSettingsSource::Config, backend);
        store.provider_secrets_initialized = true;
        install_store(cx, store);

        cx.update(|cx| SettingsStore::clear_provider_secret(id, cx));
        cx.run_until_parked();

        let persisted = parse_settings(&fs::read_to_string(config_path).unwrap()).unwrap();
        assert_eq!(persisted.provider_secret(id), Some(""));
        cx.update(|cx| {
            let store = cx.global::<SettingsStore>();
            assert!(!store.provider_secret_is_config_owned(id));
            assert!(matches!(
                store.provider_secret_status(id),
                ProviderSecretStatus::Unavailable(_)
            ));
            assert!(store.credential_warning().is_some());
        });

        fs::remove_dir_all(root).ok();
    }

    #[gpui::test]
    fn legacy_state_migrates_secrets_then_non_secret_settings(cx: &mut TestAppContext) {
        let root = test_root("migration");
        let backend = Arc::new(TestCredentialBackend::default());
        let id = ProviderSecretId::AiApiKey(AiProvider::OpenAi);
        let mut settings = Settings::default();
        settings.behavior.default_zoom = 1.5;
        settings.ai.provider_config_mut(AiProvider::OpenAi).api_key =
            "legacy-state-key".to_string();
        let state_path = root.join("state").join("config.kdl");
        write_kdl(&state_path, settings.to_kdl_string()).unwrap();
        install_store(
            cx,
            test_store(
                settings,
                &root,
                TestSettingsSource::LegacyState,
                backend.clone(),
            ),
        );

        initialize_credentials(cx);

        assert_eq!(backend.secret(id), Some("legacy-state-key".to_string()));
        assert!(!state_path.exists());
        let config_path = root.join("config").join("config.kdl");
        let config_text = fs::read_to_string(config_path).unwrap();
        assert!(!config_text.contains("legacy-state-key"));
        let persisted = parse_settings(&config_text).unwrap();
        assert_eq!(persisted.behavior.default_zoom, 1.5);
        assert_eq!(persisted.provider_secret(id), Some(""));
        cx.update(|cx| {
            assert_eq!(
                cx.global::<SettingsStore>().provider_secret_status(id),
                ProviderSecretStatus::Keychain
            );
        });

        fs::remove_dir_all(root).ok();
    }

    #[gpui::test]
    fn user_config_does_not_discard_unmigrated_state_secrets(cx: &mut TestAppContext) {
        let root = test_root("config-with-state");
        let backend = Arc::new(TestCredentialBackend::default());
        let config_id = ProviderSecretId::AiApiKey(AiProvider::OpenAi);
        let legacy_id = ProviderSecretId::TranslationApiKey(TranslationProvider::DeepL);
        let mut config_settings = Settings::default();
        config_settings
            .ai
            .provider_config_mut(AiProvider::OpenAi)
            .api_key = "configured-openai-key".to_string();
        let config_path = root.join("config").join("config.kdl");
        write_kdl(&config_path, config_settings.to_kdl_string()).unwrap();

        let mut state_settings = Settings::default();
        state_settings
            .ai
            .provider_config_mut(AiProvider::OpenAi)
            .api_key = "obsolete-openai-key".to_string();
        state_settings
            .translation
            .provider_config_mut(TranslationProvider::DeepL)
            .api_key = "unmigrated-deepl-key".to_string();
        let state_path = root.join("state").join("config.kdl");
        write_kdl(&state_path, state_settings.to_kdl_string()).unwrap();

        let mut store = test_store(
            config_settings,
            &root,
            TestSettingsSource::Config,
            backend.clone(),
        );
        store
            .settings
            .set_provider_secret(legacy_id, "unmigrated-deepl-key");
        store
            .legacy_state_secrets
            .insert(legacy_id, "unmigrated-deepl-key".to_string());
        store.stale_state_present = true;
        install_store(cx, store);

        initialize_credentials(cx);

        assert_eq!(
            backend.secret(legacy_id),
            Some("unmigrated-deepl-key".to_string())
        );
        assert!(!backend.write_urls().contains(&config_id.credential_url()));
        assert!(!state_path.exists());
        let persisted = parse_settings(&fs::read_to_string(config_path).unwrap()).unwrap();
        assert_eq!(
            persisted.provider_secret(config_id),
            Some("configured-openai-key")
        );
        assert!(
            persisted
                .provider_secret(legacy_id)
                .is_none_or(str::is_empty)
        );

        fs::remove_dir_all(root).ok();
    }

    #[gpui::test]
    fn failed_legacy_secret_migration_keeps_state_file(cx: &mut TestAppContext) {
        let root = test_root("migration-failure");
        let backend = Arc::new(TestCredentialBackend::default());
        backend.fail_writes.store(true, Ordering::Relaxed);
        let id = ProviderSecretId::AiApiKey(AiProvider::OpenAi);
        let mut settings = Settings::default();
        settings.behavior.default_zoom = 1.5;
        settings.ai.provider_config_mut(AiProvider::OpenAi).api_key =
            "legacy-state-key".to_string();
        let state_path = root.join("state").join("config.kdl");
        write_kdl(&state_path, settings.to_kdl_string()).unwrap();
        install_store(
            cx,
            test_store(settings, &root, TestSettingsSource::LegacyState, backend),
        );

        initialize_credentials(cx);

        assert!(state_path.exists());
        assert!(!root.join("config").join("config.kdl").exists());
        cx.update(|cx| {
            let store = cx.global::<SettingsStore>();
            assert!(matches!(
                store.provider_secret_status(id),
                ProviderSecretStatus::SessionOnly {
                    intended: ProviderSecretPersistence::Keychain,
                    ..
                }
            ));
            assert!(store.credential_warning().is_some());
        });

        fs::remove_dir_all(root).ok();
    }

    #[gpui::test]
    fn imported_settings_merge_credentials_and_remove_absent_providers(cx: &mut TestAppContext) {
        let root = test_root("settings-import");
        let backend = Arc::new(TestCredentialBackend::default());
        let local_id = ProviderSecretId::AiApiKey(AiProvider::OpenAi);
        let imported_id = ProviderSecretId::TranslationApiKey(TranslationProvider::DeepL);
        let mut current = Settings::default();
        current.set_provider_secret(local_id, "local-openai-key");
        current.set_provider_secret(imported_id, "configured-deepl-key");
        let removed_provider = current.ai.add_custom_provider(
            "Removed Provider",
            AiApiStyle::OpenAi,
            "https://removed.example/v1",
            "removed-provider-key",
        );
        let removed_id = ProviderSecretId::AiApiKey(removed_provider);
        backend.seed(removed_id, "removed-provider-key");
        let config_path = root.join("config").join("config.kdl");
        write_kdl(&config_path, current.to_kdl_string()).unwrap();
        let mut store = test_store(current, &root, TestSettingsSource::Config, backend.clone());
        store.provider_secrets_initialized = true;
        install_store(cx, store);

        let mut imported = Settings::default();
        imported.behavior.default_zoom = 1.5;
        imported.set_provider_secret(imported_id, " imported-deepl-key ");
        cx.update(|cx| SettingsStore::apply_imported_settings(imported, cx).unwrap());
        finish_debounced_write(cx);

        assert_eq!(
            backend.secret(imported_id),
            Some("imported-deepl-key".to_string())
        );
        assert_eq!(backend.secret(removed_id), None);
        let persisted_text = fs::read_to_string(config_path).unwrap();
        assert!(persisted_text.contains("local-openai-key"));
        assert!(!persisted_text.contains("configured-deepl-key"));
        assert!(!persisted_text.contains("imported-deepl-key"));
        assert!(!persisted_text.contains("removed-provider-key"));
        cx.update(|cx| {
            let store = cx.global::<SettingsStore>();
            assert_eq!(store.settings.behavior.default_zoom, 1.5);
            assert_eq!(
                store.settings.provider_secret(local_id),
                Some("local-openai-key")
            );
            assert_eq!(
                store.settings.provider_secret(imported_id),
                Some("imported-deepl-key")
            );
            assert!(!store.settings.ai.providers.contains_key(&removed_provider));
            assert!(store.provider_secret_is_config_owned(local_id));
            assert!(!store.provider_secret_is_config_owned(imported_id));
            assert_eq!(
                store.provider_secret_status(imported_id),
                ProviderSecretStatus::Keychain
            );
        });

        fs::remove_dir_all(root).ok();
    }

    #[gpui::test]
    fn reset_all_settings_restores_defaults_and_clears_credentials(cx: &mut TestAppContext) {
        let root = test_root("settings-reset-all");
        let backend = Arc::new(TestCredentialBackend::default());
        let builtin_id = ProviderSecretId::AiApiKey(AiProvider::OpenAi);
        let mut current = Settings::default();
        current.behavior.default_zoom = 1.5;
        current.set_provider_secret(builtin_id, "configured-openai-key");
        let custom_provider = current.ai.add_custom_provider(
            "Custom Provider",
            AiApiStyle::OpenAi,
            "https://custom.example/v1",
            "custom-provider-key",
        );
        let custom_id = ProviderSecretId::AiApiKey(custom_provider);
        backend.seed(builtin_id, "stale-keychain-copy");
        backend.seed(custom_id, "custom-provider-key");
        let config_path = root.join("config").join("config.kdl");
        write_kdl(&config_path, current.to_kdl_string()).unwrap();
        let mut store = test_store(current, &root, TestSettingsSource::Config, backend.clone());
        store.provider_secrets_initialized = true;
        install_store(cx, store);

        cx.update(|cx| SettingsStore::reset_all_settings(cx).unwrap());
        cx.run_until_parked();

        assert_eq!(backend.secret(builtin_id), None);
        assert_eq!(backend.secret(custom_id), None);
        let persisted = parse_settings(&fs::read_to_string(config_path).unwrap()).unwrap();
        assert_eq!(persisted, Settings::default());
        cx.update(|cx| {
            let store = cx.global::<SettingsStore>();
            assert_eq!(store.settings, Settings::default());
            assert!(!store.provider_secret_is_config_owned(builtin_id));
            assert_eq!(
                store.provider_secret_status(builtin_id),
                ProviderSecretStatus::Missing
            );
        });

        fs::remove_dir_all(root).ok();
    }

    #[gpui::test]
    fn failed_import_preserves_settings_and_credential_ownership(cx: &mut TestAppContext) {
        let root = test_root("settings-import-failure");
        fs::create_dir_all(&root).unwrap();
        let blocked_parent = root.join("blocked");
        fs::write(&blocked_parent, "not a directory").unwrap();
        let backend = Arc::new(TestCredentialBackend::default());
        let id = ProviderSecretId::AiApiKey(AiProvider::OpenAi);
        let mut current = Settings::default();
        current.behavior.default_zoom = 1.5;
        current.set_provider_secret(id, "configured-openai-key");
        let mut store = test_store(
            current.clone(),
            &root,
            TestSettingsSource::Config,
            backend.clone(),
        );
        store.config_path = blocked_parent.join("config.kdl");
        store.provider_secrets_initialized = true;
        install_store(cx, store);

        let mut imported = Settings::default();
        imported.behavior.default_zoom = 2.0;
        imported.set_provider_secret(id, "imported-openai-key");
        let result = cx.update(|cx| SettingsStore::apply_imported_settings(imported, cx));

        assert!(result.is_err());
        cx.run_until_parked();
        assert!(backend.write_urls().is_empty());
        cx.update(|cx| {
            let store = cx.global::<SettingsStore>();
            assert_eq!(store.settings, current);
            assert!(store.provider_secret_is_config_owned(id));
        });

        fs::remove_dir_all(root).ok();
    }

    #[gpui::test]
    fn failed_reset_preserves_settings_and_credentials(cx: &mut TestAppContext) {
        let root = test_root("settings-reset-failure");
        fs::create_dir_all(&root).unwrap();
        let blocked_parent = root.join("blocked");
        fs::write(&blocked_parent, "not a directory").unwrap();
        let backend = Arc::new(TestCredentialBackend::default());
        let id = ProviderSecretId::AiApiKey(AiProvider::OpenAi);
        backend.seed(id, "stored-openai-key");
        let mut current = Settings::default();
        current.behavior.default_zoom = 1.5;
        current.set_provider_secret(id, "configured-openai-key");
        let mut store = test_store(
            current.clone(),
            &root,
            TestSettingsSource::Config,
            backend.clone(),
        );
        store.config_path = blocked_parent.join("config.kdl");
        store.provider_secrets_initialized = true;
        install_store(cx, store);

        let result = cx.update(SettingsStore::reset_all_settings);

        assert!(result.is_err());
        cx.run_until_parked();
        assert_eq!(backend.secret(id), Some("stored-openai-key".to_string()));
        cx.update(|cx| {
            let store = cx.global::<SettingsStore>();
            assert_eq!(store.settings, current);
            assert!(store.provider_secret_is_config_owned(id));
        });

        fs::remove_dir_all(root).ok();
    }
}
