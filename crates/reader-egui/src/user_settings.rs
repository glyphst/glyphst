//! The user's settings (providers, models, keys), shared with the GPUI
//! Reader: read on a thread at startup, edited in the settings window and
//! saved shortly after the last edit, on a worker thread. Keys the config
//! file held stay there; others go to the system keychain, as the GPUI
//! Reader keeps them.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use glyphst_settings::{
    CredentialStore, MemoryCredentials, ProviderSecretId, Settings, SystemKeychain,
};

/// Edits are saved this long after the last one.
const SAVE_DELAY: Duration = Duration::from_millis(500);

/// Where edits go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Persistence {
    /// The config file and the system keychain.
    User,
    /// Nowhere but memory: scripted runs, fakes and tests, so fake
    /// providers and keys never reach the user's files.
    Memory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretStatus {
    Loading,
    InConfig,
    InKeychain,
    Missing,
    Saving,
    /// Kept for this session only.
    NotSaved(String),
}

impl SecretStatus {
    pub fn label(&self) -> String {
        match self {
            Self::Loading => "Loading…".into(),
            Self::InConfig => "Saved in the config file".into(),
            Self::InKeychain => "Saved in the keychain".into(),
            Self::Missing => "Not set".into(),
            Self::Saving => "Saving…".into(),
            Self::NotSaved(error) => format!("Not saved: {error}"),
        }
    }
}

struct Loaded {
    settings: Settings,
    config_owned: BTreeSet<ProviderSecretId>,
    warnings: Vec<String>,
}

struct SaveJob {
    settings: Settings,
    config_owned: BTreeSet<ProviderSecretId>,
    /// Keychain secrets changed since the last save; empty ones are
    /// removed.
    secrets: Vec<(ProviderSecretId, String)>,
}

enum Saved {
    Config(Result<(), String>),
    Secret(ProviderSecretId, Result<bool, String>),
}

pub struct UserSettings {
    persistence: Persistence,
    /// The settings in effect, sanitized as they are saved.
    settings: Option<Arc<Settings>>,
    /// The settings as the window edits them: fields keep what is typed
    /// (spaces, a host's trailing slash) until saved.
    draft: Option<Arc<Settings>>,
    config_owned: BTreeSet<ProviderSecretId>,
    loading: Option<Receiver<Loaded>>,
    /// Problems reading or saving the settings and keys.
    pub warnings: Vec<String>,
    store: Arc<dyn CredentialStore>,
    statuses: BTreeMap<ProviderSecretId, SecretStatus>,
    /// Keychain secrets as last saved, to find what an edit changed.
    saved_secrets: BTreeMap<ProviderSecretId, String>,
    /// Saves of each secret not yet reported.
    in_flight: BTreeMap<ProviderSecretId, usize>,
    edited_at: Option<Instant>,
    jobs: Option<Sender<SaveJob>>,
    saved_tx: Sender<Saved>,
    saved_rx: Receiver<Saved>,
    ctx: egui::Context,
}

impl UserSettings {
    /// Starts reading the user's settings and keychain keys on a thread.
    pub fn load(ctx: &egui::Context) -> Self {
        let (sender, receiver) = mpsc::channel();
        let repaint = ctx.clone();
        let spawned = thread::Builder::new()
            .name("settings".into())
            .spawn(move || {
                let mut settings = glyphst_settings::load_user_settings();
                let config_owned = glyphst_settings::config_owned_secret_ids(&settings);
                let warnings =
                    glyphst_settings::read_keychain_secrets(&mut settings, &SystemKeychain);
                let _ = sender.send(Loaded {
                    settings,
                    config_owned,
                    warnings,
                });
                repaint.request_repaint();
            });
        let mut user = Self::new(ctx, Persistence::User, Arc::new(SystemKeychain));
        match spawned {
            Ok(_) => user.loading = Some(receiver),
            Err(error) => user
                .warnings
                .push(format!("could not read the settings: {error}")),
        }
        user
    }

    /// Settings kept in memory only, with keys in `store`.
    pub fn in_memory(
        ctx: &egui::Context,
        settings: Settings,
        store: Arc<MemoryCredentials>,
    ) -> Self {
        let mut user = Self::new(ctx, Persistence::Memory, store);
        user.loaded(Loaded {
            settings,
            config_owned: BTreeSet::new(),
            warnings: Vec::new(),
        });
        user
    }

    fn new(ctx: &egui::Context, persistence: Persistence, store: Arc<dyn CredentialStore>) -> Self {
        let (saved_tx, saved_rx) = mpsc::channel();
        Self {
            persistence,
            settings: None,
            draft: None,
            config_owned: BTreeSet::new(),
            loading: None,
            warnings: Vec::new(),
            store,
            statuses: BTreeMap::new(),
            saved_secrets: BTreeMap::new(),
            in_flight: BTreeMap::new(),
            edited_at: None,
            jobs: None,
            saved_tx,
            saved_rx,
            ctx: ctx.clone(),
        }
    }

    fn loaded(&mut self, loaded: Loaded) {
        let settings = loaded.settings;
        self.config_owned = loaded.config_owned;
        self.warnings.extend(loaded.warnings);
        self.saved_secrets.clear();
        self.statuses.clear();
        for id in settings.provider_secret_ids() {
            let secret = secret_of(&settings, id);
            let status = if self.config_owned.contains(&id) {
                SecretStatus::InConfig
            } else if secret.is_empty() {
                SecretStatus::Missing
            } else {
                SecretStatus::InKeychain
            };
            self.statuses.insert(id, status);
            if !self.config_owned.contains(&id) {
                self.saved_secrets.insert(id, secret);
            }
        }
        let settings = Arc::new(settings);
        self.draft = Some(settings.clone());
        self.settings = Some(settings);
    }

    /// The settings, once read.
    pub fn get(&self) -> Option<&Arc<Settings>> {
        self.settings.as_ref()
    }

    /// The settings as the window edits them.
    pub fn draft(&self) -> Option<&Arc<Settings>> {
        self.draft.as_ref()
    }

    pub fn persistence(&self) -> Persistence {
        self.persistence
    }

    pub fn status(&self, id: ProviderSecretId) -> SecretStatus {
        self.statuses
            .get(&id)
            .cloned()
            .unwrap_or(if self.settings.is_some() {
                SecretStatus::Missing
            } else {
                SecretStatus::Loading
            })
    }

    /// Applies an edit now and saves it shortly. Does nothing before the
    /// settings are read.
    pub fn edit(&mut self, edit: impl FnOnce(&mut Settings)) {
        let Some(current) = &self.draft else {
            return;
        };
        let mut draft = Settings::clone(current);
        edit(&mut draft);
        if draft == **current {
            return;
        }
        let mut settings = draft.clone();
        settings.sanitize();
        self.draft = Some(Arc::new(draft));
        for id in settings.provider_secret_ids() {
            if self.config_owned.contains(&id) {
                continue;
            }
            if self.saved_secrets.get(&id) != Some(&secret_of(&settings, id)) {
                self.statuses.insert(id, SecretStatus::Saving);
            }
        }
        self.settings = Some(Arc::new(settings));
        self.edited_at = Some(Instant::now());
        self.ctx.request_repaint_after(SAVE_DELAY);
    }

    /// Takes the loaded settings and save results; starts a save once
    /// edits settle.
    pub fn poll(&mut self, now: Instant) {
        if let Some(loaded) = self
            .loading
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
        {
            self.loading = None;
            self.loaded(loaded);
        }
        while let Ok(saved) = self.saved_rx.try_recv() {
            match saved {
                Saved::Config(Ok(())) => {}
                Saved::Config(Err(error)) => {
                    self.warnings
                        .push(format!("could not save the settings: {error}"));
                }
                Saved::Secret(id, result) => {
                    let pending = self.in_flight.entry(id).or_default();
                    *pending = pending.saturating_sub(1);
                    // A later save of it, or an edit not saved yet, will
                    // report.
                    let unsaved = self.settings.as_ref().is_some_and(|settings| {
                        self.saved_secrets.get(&id) != Some(&secret_of(settings, id))
                    });
                    if *pending > 0 || unsaved {
                        continue;
                    }
                    let status = match result {
                        Ok(true) => SecretStatus::InKeychain,
                        Ok(false) => SecretStatus::Missing,
                        Err(error) => SecretStatus::NotSaved(error),
                    };
                    self.statuses.insert(id, status);
                }
            }
        }
        match self.edited_at {
            Some(at) if now.duration_since(at) >= SAVE_DELAY => {
                self.edited_at = None;
                self.save();
            }
            Some(at) => self
                .ctx
                .request_repaint_after(SAVE_DELAY.saturating_sub(now.duration_since(at))),
            None => {}
        }
    }

    fn save(&mut self) {
        let Some(settings) = self.settings.clone() else {
            return;
        };
        let mut secrets = Vec::new();
        for id in settings.provider_secret_ids() {
            if self.config_owned.contains(&id) {
                continue;
            }
            let secret = secret_of(&settings, id);
            if self.saved_secrets.get(&id) != Some(&secret) {
                self.saved_secrets.insert(id, secret.clone());
                *self.in_flight.entry(id).or_default() += 1;
                secrets.push((id, secret));
            }
        }
        let job = SaveJob {
            settings: Settings::clone(&settings),
            config_owned: self.config_owned.clone(),
            secrets,
        };
        if self.jobs.is_none() {
            self.jobs = self.start_worker();
        }
        if let Some(jobs) = &self.jobs
            && jobs.send(job).is_err()
        {
            self.warnings.push("could not save the settings".into());
        }
    }

    /// One worker saves in order, so a later edit is never overwritten by
    /// an earlier one.
    fn start_worker(&mut self) -> Option<Sender<SaveJob>> {
        let (sender, receiver) = mpsc::channel::<SaveJob>();
        let saved = self.saved_tx.clone();
        let store = self.store.clone();
        let persistence = self.persistence;
        let ctx = self.ctx.clone();
        let spawned = thread::Builder::new()
            .name("settings-save".into())
            .spawn(move || {
                for job in receiver {
                    if persistence == Persistence::User {
                        let result =
                            glyphst_settings::save_user_settings(&job.settings, &job.config_owned)
                                .map_err(|error| format!("{error:#}"));
                        let _ = saved.send(Saved::Config(result));
                    }
                    for (id, secret) in job.secrets {
                        let result = if secret.is_empty() {
                            store.delete(id).map(|()| false)
                        } else {
                            store.write(id, &secret).map(|()| true)
                        };
                        let _ = saved.send(Saved::Secret(id, result.map_err(|e| format!("{e:#}"))));
                    }
                    ctx.request_repaint();
                }
            });
        match spawned {
            Ok(_) => Some(sender),
            Err(error) => {
                self.warnings
                    .push(format!("could not save the settings: {error}"));
                None
            }
        }
    }
}

/// A secret as it is saved: trimmed, empty when unset.
fn secret_of(settings: &Settings, id: ProviderSecretId) -> String {
    settings
        .provider_secret(id)
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use glyphst_settings::TranslationProvider;

    use super::*;

    #[test]
    fn edited_keys_are_saved_to_the_store_after_a_pause() {
        let ctx = egui::Context::default();
        let store = Arc::new(MemoryCredentials::default());
        let mut user = UserSettings::in_memory(&ctx, Settings::default(), store.clone());
        assert_eq!(user.persistence(), Persistence::Memory);
        let deepl = ProviderSecretId::TranslationApiKey(TranslationProvider::DeepL);
        assert_eq!(user.status(deepl), SecretStatus::Missing);

        user.edit(|settings| {
            settings.translation.active_provider = TranslationProvider::DeepL;
            settings.set_provider_secret(deepl, "secret-key");
        });
        assert_eq!(user.status(deepl), SecretStatus::Saving);
        assert_eq!(
            user.get().map(|s| s.translation.active_provider),
            Some(TranslationProvider::DeepL),
            "edits apply at once"
        );

        let start = Instant::now();
        user.poll(start);
        assert_eq!(store.get(deepl), None, "not before the pause");
        let deadline = start + Duration::from_secs(5);
        let mut now = start + SAVE_DELAY;
        while user.status(deepl) == SecretStatus::Saving && Instant::now() < deadline {
            user.poll(now);
            now += Duration::from_millis(10);
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(user.status(deepl), SecretStatus::InKeychain);
        assert_eq!(store.get(deepl).as_deref(), Some("secret-key"));

        user.edit(|settings| {
            settings.set_provider_secret(deepl, "");
        });
        let mut now = Instant::now() + SAVE_DELAY;
        let deadline = Instant::now() + Duration::from_secs(5);
        while user.status(deepl) == SecretStatus::Saving && Instant::now() < deadline {
            user.poll(now);
            now += Duration::from_millis(10);
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(user.status(deepl), SecretStatus::Missing);
        assert_eq!(store.get(deepl), None, "a cleared key is removed");
        assert!(user.warnings.is_empty());
    }
}
