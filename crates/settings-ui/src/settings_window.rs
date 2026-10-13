mod fields;
mod pages;

use glyphst_actions::settings_panel::PanelClose;
use glyphst_ai::ai_client::{AiModelInfo, list_ai_models, test_ai_connection};
use glyphst_ai::chatgpt_subscription::ChatGptSubscription;
use glyphst_files::path_chooser::{
    OpenPathOptions, PathFilter, SavePathOptions, prompt_for_new_path, prompt_for_paths,
};
use glyphst_keymap::keybindings;
use glyphst_pdf_translation_service::{
    OcrProbeError, test_ocr_connection as test_ocr_connection_request,
};
use glyphst_settings::{
    AiModel, AiProvider, ModelRef, OcrProvider, ProviderSecretId, ResolvedAiConfig, Settings,
    SettingsStore, WindowSettings, localize, parse_settings, setup_ui_font, sync_component_theme,
    ui_rems,
};
use glyphst_translation_ui::TranslationServices;
use glyphst_ui::{
    activity::{ActivityCenter, ActivityNoticeEntry, ActivityNoticeLevel, BackgroundTaskEntry},
    root::RootWindowHandle,
    style::{SURFACE_INSET, apply_window_background, shell_background},
};
use gpui::{
    App, AppContext, Bounds, Context, Entity, FocusHandle, Focusable, IntoElement, PromptButton,
    PromptLevel, Render, SharedString, Subscription, Task, Window, WindowBounds, WindowOptions,
    div, prelude::*, px, size,
};
use gpui_component::{
    ActiveTheme as _, Root, Sizable as _, Size, TitleBar, WindowExt as _,
    group_box::GroupBoxVariant,
    notification::Notification,
    setting::{SelectIndex, Settings as ComponentSettings},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

pub fn open_settings_window(open_keymap: fn(&mut App), app_id: &'static str, cx: &mut App) {
    if let Some(existing_window) = cx
        .windows()
        .into_iter()
        .find_map(|window| RootWindowHandle::<SettingsWindow>::from_any(window, cx))
    {
        if let Err(error) = existing_window.update(cx, |_, window, _| {
            window.activate_window();
        }) {
            eprintln!("failed to activate settings window: {error:#}");
        }
        return;
    }

    let window_settings = cx.global::<SettingsStore>().settings.window;
    let transparency_enabled = window_settings.transparency_enabled_by_default();
    let bounds = Bounds::centered(None, size(px(900.0), px(760.0)), cx);

    if let Err(error) = cx.open_window(
        WindowOptions {
            titlebar: Some(TitleBar::title_bar_options()),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_background: window_settings.background_appearance(transparency_enabled),
            app_id: Some(app_id.to_string()),
            window_min_size: Some(size(px(720.0), px(420.0))),
            ..Default::default()
        },
        move |window, cx| {
            window.set_app_id(app_id);
            let settings = cx.new(|cx| SettingsWindow::new(open_keymap, cx));
            cx.new(|cx| Root::new(settings, window, cx))
        },
    ) {
        eprintln!("failed to open settings window: {error:#}");
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct ModelRequestSignature {
    provider: AiProvider,
    base_url: String,
    has_api_key: bool,
}

#[derive(Default)]
pub(super) struct ProviderOperation {
    provider: Option<AiProvider>,
    signature: Option<ModelRequestSignature>,
    loading: bool,
    status: Option<SharedString>,
    failed: bool,
}

#[derive(Clone)]
pub(super) struct CachedModelCatalog {
    signature: ModelRequestSignature,
    models: Vec<AiModelInfo>,
}

#[derive(Default)]
pub(super) struct ModelCatalogOperation {
    provider: Option<AiProvider>,
    signature: Option<ModelRequestSignature>,
    loading: bool,
    status: Option<SharedString>,
    failed: bool,
}

#[derive(Default)]
pub(super) struct ModelPickerState {
    provider: Option<AiProvider>,
    selected: BTreeSet<String>,
    manual: BTreeSet<String>,
}

pub(super) struct ProviderRenameState {
    provider: AiProvider,
    input: Entity<gpui_component::input::InputState>,
    _subscription: Subscription,
}

#[derive(Clone)]
pub(super) enum OcrConnectionResult {
    Testing,
    Succeeded,
    InvalidAuth,
    Retryable,
    ProviderError(Option<String>),
}

#[derive(Clone)]
pub(super) struct OcrOperation {
    provider: OcrProvider,
    result: OcrConnectionResult,
}

pub struct SettingsWindow {
    focus_handle: FocusHandle,
    pub(super) open_keymap: fn(&mut App),
    pub(super) font_families: Vec<String>,
    initial_selection: SelectIndex,
    status: Option<SharedString>,
    save_error: Option<SharedString>,
    save_generation: Arc<AtomicU64>,
    pub(super) provider_operation: ProviderOperation,
    _provider_task: Option<Task<()>>,
    pub(super) model_catalogs: BTreeMap<AiProvider, CachedModelCatalog>,
    pub(super) model_catalog_operation: ModelCatalogOperation,
    pub(super) model_picker: ModelPickerState,
    _model_catalog_task: Option<Task<()>>,
    pub(super) provider_rename: Option<ProviderRenameState>,
    pub(super) active_model_drag: Option<AiProvider>,
    pub(super) ocr_operation: Option<OcrOperation>,
    _ocr_task: Option<Task<()>>,
    pub(super) translation_data_clear_pending: bool,
    pub(super) translation_data_clear_running: bool,
    window_settings: WindowSettings,
    transparency_enabled: bool,
}

impl SettingsWindow {
    pub fn new(open_keymap: fn(&mut App), cx: &mut Context<Self>) -> Self {
        let font_settings = &cx.global::<SettingsStore>().settings.font;
        let window_settings = cx.global::<SettingsStore>().settings.window;
        let mut font_families = cx.text_system().all_font_names();
        font_families.extend(
            [".ZedSans", ".ZedMono", ".SystemUIFont"]
                .into_iter()
                .map(str::to_string),
        );
        font_families.push(font_settings.ui_font_family.clone());
        font_families.extend(font_settings.ui_font_fallbacks.clone());
        font_families.extend(Settings::default().font.ui_font_fallbacks);
        font_families.sort_by_cached_key(|font| font.to_ascii_lowercase());
        font_families.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

        Self {
            focus_handle: cx.focus_handle(),
            open_keymap,
            font_families,
            initial_selection: SelectIndex::default(),
            status: None,
            save_error: None,
            save_generation: Arc::new(AtomicU64::new(0)),
            provider_operation: ProviderOperation::default(),
            _provider_task: None,
            model_catalogs: BTreeMap::new(),
            model_catalog_operation: ModelCatalogOperation::default(),
            model_picker: ModelPickerState::default(),
            _model_catalog_task: None,
            provider_rename: None,
            active_model_drag: None,
            ocr_operation: None,
            _ocr_task: None,
            translation_data_clear_pending: false,
            translation_data_clear_running: false,
            window_settings,
            transparency_enabled: window_settings.transparency_enabled_by_default(),
        }
    }

    pub(super) fn provider_secret_description(id: ProviderSecretId, cx: &App) -> SharedString {
        let status = cx.global::<SettingsStore>().provider_secret_status(id);
        match status.error() {
            Some(error) => format!("{}: {error}", status.source_label()).into(),
            None => status.source_label().into(),
        }
    }

    pub(super) fn setting_option(
        value: impl Into<SharedString>,
        label: impl Into<SharedString>,
    ) -> (SharedString, SharedString) {
        (value.into(), label.into())
    }

    pub(super) fn model_ref_key(model: &ModelRef) -> String {
        format!("{}::{}", model.provider.as_str(), model.id)
    }

    pub(super) fn parse_model_ref(value: &str) -> Option<ModelRef> {
        let (provider, id) = value.split_once("::")?;
        let provider = AiProvider::parse(provider)?;
        (!id.trim().is_empty()).then(|| ModelRef::new(provider, id))
    }

    pub(super) fn model_options(settings: &Settings) -> Vec<(SharedString, SharedString)> {
        let mut options = Vec::new();
        for provider in settings.ai.ordered_providers() {
            let config = settings.ai.provider_config(*provider);
            if !config.enabled {
                continue;
            }
            options.extend(config.models.into_iter().map(|model| {
                Self::setting_option(
                    Self::model_ref_key(&ModelRef::new(*provider, model.id)),
                    format!("{} — {}", config.name, model.name),
                )
            }));
        }
        if options.is_empty() {
            options.push(Self::setting_option(
                "",
                crate::fl!("settings-no-enabled-provider-models"),
            ));
        }
        options
    }

    pub(super) fn parse_font_fallbacks(value: &str) -> Vec<String> {
        value
            .split(',')
            .map(str::trim)
            .filter(|fallback| !fallback.is_empty())
            .map(str::to_string)
            .collect()
    }

    pub(super) fn font_fallbacks(settings: &Settings) -> SharedString {
        settings.font.ui_font_fallbacks.join(", ").into()
    }

    fn apply_runtime_changes(before: &Settings, after: &Settings, cx: &mut App) -> bool {
        if before.behavior.reduce_motion != after.behavior.reduce_motion {
            cx.set_reduce_motion(after.behavior.reduce_motion);
        }
        if before.keymap_preset != after.keymap_preset
            || before.keybinding_overrides != after.keybinding_overrides
        {
            keybindings::apply(cx, after.keymap_preset, &after.keybinding_overrides);
        }
        if before.language != after.language {
            localize::select_language(after.language);
        }
        if before.theme != after.theme
            || before.overrides != after.overrides
            || before.font != after.font
        {
            sync_component_theme(cx);
            true
        } else {
            false
        }
    }

    fn persist_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        self.save_generation.fetch_add(1, Ordering::Relaxed);
        let before = cx.global::<SettingsStore>().settings.clone();
        let result = cx.global_mut::<SettingsStore>().set_settings(settings);
        let after = cx.global::<SettingsStore>().settings.clone();
        if !Self::apply_runtime_changes(&before, &after, cx) {
            cx.refresh_windows();
        }
        self.status = None;
        self.save_error = result.err().map(|error| format!("{error:#}").into());
        cx.notify();
    }

    fn finish_settings_replacement(
        &mut self,
        before: Settings,
        result: std::result::Result<(), String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let succeeded = result.is_ok();
        let after = cx.global::<SettingsStore>().settings.clone();
        if succeeded {
            self.save_generation.fetch_add(1, Ordering::Relaxed);
            self._provider_task.take();
            self.provider_operation = ProviderOperation::default();
            self.model_catalogs.clear();
            self._model_catalog_task.take();
            self.model_catalog_operation = ModelCatalogOperation::default();
            self.model_picker = ModelPickerState::default();
            self.provider_rename = None;
            self.active_model_drag = None;
            self._ocr_task.take();
            self.ocr_operation = None;
            self.font_families.push(after.font.ui_font_family.clone());
            self.font_families
                .extend(after.font.ui_font_fallbacks.iter().cloned());
            self.font_families
                .sort_by_cached_key(|font| font.to_ascii_lowercase());
            self.font_families
                .dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        }
        if !Self::apply_runtime_changes(&before, &after, cx) {
            cx.refresh_windows();
        }
        self.status = None;
        self.save_error = result.err().map(Into::into);
        cx.notify();
        succeeded
    }

    pub(super) fn update_settings(
        view: &Entity<Self>,
        cx: &mut App,
        update: impl FnOnce(&mut Settings),
    ) {
        view.update(cx, |this, cx| {
            let mut settings = cx.global::<SettingsStore>().settings.clone();
            update(&mut settings);
            this.persist_settings(settings, cx);
        });
    }

    pub(super) fn update_text_settings(
        view: &Entity<Self>,
        cx: &mut App,
        update: impl FnOnce(&mut Settings),
    ) {
        view.update(cx, |this, cx| {
            let before = cx.global::<SettingsStore>().settings.clone();
            let mut settings = before.clone();
            update(&mut settings);
            cx.global_mut::<SettingsStore>()
                .set_settings_in_memory(settings);
            let after = cx.global::<SettingsStore>().settings.clone();
            Self::apply_runtime_changes(&before, &after, cx);

            this.status = None;
            this.save_error = None;
            let generation = this
                .save_generation
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1);
            let save_generation = this.save_generation.clone();
            let (path, contents) = cx.global::<SettingsStore>().persistence_snapshot();
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(300))
                    .await;
                if save_generation.load(Ordering::Relaxed) != generation {
                    return;
                }
                // Keep the final generation check and write in one foreground
                // turn. Otherwise an older background write can race a newer
                // immediate settings save and win on disk. The settings file
                // is small, so this bounded write is preferable to that race.
                let result = SettingsStore::write_snapshot(&path, contents)
                    .map_err(|error| format!("{error:#}"));
                _ = this.update(cx, |this, cx| {
                    if result.is_ok() {
                        cx.global_mut::<SettingsStore>()
                            .settings_snapshot_written(&path);
                    }
                    this.save_error = result.err().map(Into::into);
                    cx.notify();
                });
            })
            .detach();
            cx.notify();
        });
    }

    pub(super) fn resolved_ai_config(
        settings: &Settings,
        provider: AiProvider,
    ) -> ResolvedAiConfig {
        settings.ai.resolved_provider(provider)
    }

    fn model_request_signature(config: &ResolvedAiConfig) -> ModelRequestSignature {
        ModelRequestSignature {
            provider: config.provider,
            base_url: config.base_url.clone(),
            has_api_key: !config.api_key.is_empty(),
        }
    }

    fn current_model_request_signature(provider: AiProvider, cx: &App) -> ModelRequestSignature {
        let config = Self::resolved_ai_config(&cx.global::<SettingsStore>().settings, provider);
        Self::model_request_signature(&config)
    }

    pub(super) fn set_model_picker_open(
        &mut self,
        provider: AiProvider,
        open: bool,
        cx: &mut Context<Self>,
    ) {
        if !open {
            if self.model_picker.provider == Some(provider) {
                self.model_picker = ModelPickerState::default();
                cx.notify();
            }
            return;
        }

        self.model_picker = ModelPickerState {
            provider: Some(provider),
            selected: BTreeSet::new(),
            manual: BTreeSet::new(),
        };

        let signature = Self::current_model_request_signature(provider, cx);
        let has_current_cache = self
            .model_catalogs
            .get(&provider)
            .is_some_and(|catalog| catalog.signature == signature);
        if !has_current_cache {
            self.model_catalogs.remove(&provider);
        }
        let operation_matches = self.model_catalog_operation.provider == Some(provider)
            && self.model_catalog_operation.signature.as_ref() == Some(&signature);
        if !operation_matches && self.model_catalog_operation.provider == Some(provider) {
            self._model_catalog_task.take();
            self.model_catalog_operation = ModelCatalogOperation::default();
        }
        cx.notify();
    }

    pub(super) fn fetch_provider_models(&mut self, provider: AiProvider, cx: &mut Context<Self>) {
        if cx
            .global::<SettingsStore>()
            .provider_secret_is_loading(ProviderSecretId::AiApiKey(provider))
        {
            self.model_catalog_operation = ModelCatalogOperation {
                provider: Some(provider),
                loading: false,
                status: Some("Credentials are still loading.".into()),
                ..ModelCatalogOperation::default()
            };
            cx.notify();
            return;
        }
        let config = Self::resolved_ai_config(&cx.global::<SettingsStore>().settings, provider);
        let signature = Self::model_request_signature(&config);
        self.model_catalog_operation = ModelCatalogOperation {
            provider: Some(provider),
            signature: Some(signature.clone()),
            loading: true,
            status: Some(crate::fl!("settings-fetching-models").into()),
            ..ModelCatalogOperation::default()
        };
        cx.notify();

        self._model_catalog_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    list_ai_models(config).map_err(|error| format!("{error:#}"))
                })
                .await;
            _ = this.update(cx, |this, cx| {
                if this.model_catalog_operation.signature.as_ref() != Some(&signature) {
                    return;
                }
                if Self::current_model_request_signature(provider, cx) != signature {
                    this.model_catalog_operation.loading = false;
                    this.model_catalog_operation.failed = false;
                    this.model_catalog_operation.status =
                        Some("Provider settings changed. Fetch models again.".into());
                    cx.notify();
                    return;
                }
                let operation = &mut this.model_catalog_operation;
                operation.loading = false;
                match result {
                    Ok(models) => {
                        let existing = cx
                            .global::<SettingsStore>()
                            .settings
                            .ai
                            .provider_config(provider)
                            .models
                            .into_iter()
                            .map(|model| model.id)
                            .collect::<BTreeSet<_>>();
                        if this.model_picker.provider == Some(provider) {
                            this.model_picker.selected.retain(|model_id| {
                                !existing.contains(model_id)
                                    && models.iter().any(|model| model.id == *model_id)
                            });
                        }
                        let count = models.len();
                        this.model_catalogs.insert(
                            provider,
                            CachedModelCatalog {
                                signature: signature.clone(),
                                models,
                            },
                        );
                        operation.failed = false;
                        operation.status = Some(
                            crate::fl!("settings-fetched-models", count = count as i64).into(),
                        );
                    }
                    Err(error) => {
                        operation.failed = true;
                        operation.status = Some(
                            crate::fl!("settings-fetch-models-failed", error = error.as_str())
                                .into(),
                        );
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn test_provider_connection(
        &mut self,
        provider: AiProvider,
        cx: &mut Context<Self>,
    ) {
        if cx
            .global::<SettingsStore>()
            .provider_secret_is_loading(ProviderSecretId::AiApiKey(provider))
        {
            self.provider_operation = ProviderOperation {
                provider: Some(provider),
                loading: false,
                status: Some("Credentials are still loading.".into()),
                failed: false,
                ..ProviderOperation::default()
            };
            cx.notify();
            return;
        }
        let config = Self::resolved_ai_config(&cx.global::<SettingsStore>().settings, provider);
        let signature = Self::model_request_signature(&config);
        self.provider_operation = ProviderOperation {
            provider: Some(provider),
            signature: Some(signature.clone()),
            loading: true,
            status: Some(crate::fl!("settings-testing-connection").into()),
            ..ProviderOperation::default()
        };
        cx.notify();

        self._provider_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    test_ai_connection(config).map_err(|error| format!("{error:#}"))
                })
                .await;
            _ = this.update(cx, |this, cx| {
                if this.provider_operation.signature.as_ref() != Some(&signature) {
                    return;
                }
                if Self::current_model_request_signature(provider, cx) != signature {
                    this.provider_operation.loading = false;
                    this.provider_operation.failed = false;
                    this.provider_operation.status =
                        Some("Provider settings changed. Test the connection again.".into());
                    cx.notify();
                    return;
                }
                let operation = &mut this.provider_operation;
                operation.loading = false;
                operation.failed = result.is_err();
                operation.status = Some(match result {
                    Ok(()) => crate::fl!("settings-connection-succeeded").into(),
                    Err(error) => {
                        crate::fl!("settings-connection-failed", error = error.as_str()).into()
                    }
                });
                cx.notify();
            });
        }));
    }

    pub(super) fn toggle_model_picker_model(&mut self, model_id: String) {
        if self.model_picker.manual.remove(&model_id) {
            return;
        }
        if self.model_picker.selected.contains(&model_id) {
            self.model_picker.selected.remove(&model_id);
        } else {
            self.model_picker.selected.insert(model_id);
        }
    }

    pub(super) fn stage_manual_model_id(&mut self, provider: AiProvider, model_id: String) -> bool {
        if self.model_picker.provider != Some(provider)
            || self.model_picker.selected.contains(&model_id)
            || self.model_picker.manual.contains(&model_id)
        {
            return false;
        }
        self.model_picker.manual.insert(model_id)
    }

    pub(super) fn remove_manual_model_id(&mut self, model_id: &str) {
        self.model_picker.manual.remove(model_id);
    }

    pub(super) fn clear_model_picker_selection(&mut self) {
        self.model_picker.selected.clear();
        self.model_picker.manual.clear();
    }

    pub(super) fn invalidate_provider_operation(&mut self, provider: AiProvider) {
        if self.provider_operation.provider == Some(provider) {
            self._provider_task.take();
            self.provider_operation = ProviderOperation::default();
        }
        if self.model_catalog_operation.provider == Some(provider) {
            self._model_catalog_task.take();
            self.model_catalog_operation = ModelCatalogOperation::default();
        }
        self.model_catalogs.remove(&provider);
        if self.model_picker.provider == Some(provider) {
            self.clear_model_picker_selection();
        }
    }

    pub(super) fn set_visible_model_selection(
        &mut self,
        model_ids: impl IntoIterator<Item = String>,
        selected: bool,
    ) {
        if selected {
            self.model_picker.selected.extend(model_ids);
        } else {
            for model_id in model_ids {
                self.model_picker.selected.remove(&model_id);
            }
        }
    }

    pub(super) fn add_selected_models(&mut self, provider: AiProvider, cx: &mut Context<Self>) {
        let selected = self.model_picker.selected.clone();
        let manual = self.model_picker.manual.clone();
        let discovered = self
            .model_catalogs
            .get(&provider)
            .map(|catalog| catalog.models.clone())
            .unwrap_or_default();
        if selected.is_empty() && manual.is_empty() {
            return;
        }

        let mut settings = cx.global::<SettingsStore>().settings.clone();
        let config = settings.ai.provider_config_mut(provider);
        let pending = selected.union(&manual).cloned().collect::<BTreeSet<_>>();
        let mut added = 0usize;
        for model_id in pending {
            let model = discovered.iter().find(|model| model.id == model_id);
            let (name, group) = if let Some(model) = model {
                (
                    model.label.clone(),
                    model
                        .owned_by
                        .clone()
                        .unwrap_or_else(|| config.name.clone()),
                )
            } else if manual.contains(&model_id) {
                (model_id.clone(), config.name.clone())
            } else {
                continue;
            };
            let old_len = config.models.len();
            config.ensure_model(AiModel::new(model_id, name, group));
            added += usize::from(config.models.len() != old_len);
        }
        self.persist_settings(settings, cx);
        self.clear_model_picker_selection();
        self.status = Some(crate::fl!("settings-added-models", count = added as i64).into());
        cx.notify();
    }

    pub(super) fn test_ocr_connection(&mut self, provider: OcrProvider, cx: &mut Context<Self>) {
        let credentials_loading = match provider {
            OcrProvider::Paddle => cx
                .global::<SettingsStore>()
                .provider_secret_is_loading(ProviderSecretId::PaddleOcrToken),
            OcrProvider::Unlimited => {
                let store = cx.global::<SettingsStore>();
                store.provider_secret_is_loading(ProviderSecretId::UnlimitedOcrApiKey)
                    || store.provider_secret_is_loading(ProviderSecretId::UnlimitedOcrSecretKey)
            }
        };
        if credentials_loading {
            self.save_error = Some("Credentials are still loading.".into());
            cx.notify();
            return;
        }
        let mut settings = cx.global::<SettingsStore>().settings.ocr.clone();
        settings.provider = provider;
        self.ocr_operation = Some(OcrOperation {
            provider,
            result: OcrConnectionResult::Testing,
        });
        cx.notify();

        let request_settings = settings.clone();
        self._ocr_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { test_ocr_connection_request(request_settings) })
                .await;
            _ = this.update(cx, |this, cx| {
                let mut current = cx.global::<SettingsStore>().settings.ocr.clone();
                current.provider = provider;
                if current != settings {
                    return;
                }
                this.ocr_operation = Some(OcrOperation {
                    provider,
                    result: match result {
                        Ok(()) => OcrConnectionResult::Succeeded,
                        Err(OcrProbeError::InvalidAuth) => OcrConnectionResult::InvalidAuth,
                        Err(OcrProbeError::Retryable) => OcrConnectionResult::Retryable,
                        Err(OcrProbeError::Provider(code)) => {
                            OcrConnectionResult::ProviderError(code)
                        }
                    },
                });
                cx.notify();
            });
        }));
    }

    pub(super) fn clear_translation_data(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.translation_data_clear_pending {
            return;
        }
        if !cx.has_global::<TranslationServices>() || ActivityCenter::try_global(cx).is_none() {
            self.status = None;
            self.save_error = Some("Translation data service is unavailable.".into());
            cx.notify();
            return;
        }

        let service = cx.global::<TranslationServices>().pdf().clone();
        let activity_center = ActivityCenter::global(cx);
        let confirmation = window.prompt(
            PromptLevel::Warning,
            "Clear all translation data?",
            Some(
                "This deletes cached provider responses, generated translated PDFs, OCR results, \
                 and resumable checkpoints. Source PDFs and settings are not changed.",
            ),
            &[
                PromptButton::ok("Clear translation data"),
                PromptButton::cancel("Cancel"),
            ],
            cx,
        );
        self.status = None;
        self.save_error = None;
        self.translation_data_clear_pending = true;
        cx.spawn_in(window, async move |this, cx| {
            if confirmation.await != Ok(0) {
                _ = this.update_in(cx, |this, _, cx| {
                    this.translation_data_clear_pending = false;
                    this.translation_data_clear_running = false;
                    cx.notify();
                });
                return;
            }

            let task_id = activity_center.update(cx, |center, cx| {
                center.add_task(
                    BackgroundTaskEntry::new(
                        "Clearing translation data",
                        "Removing cached responses, generated PDFs, OCR results, and checkpoints.",
                    ),
                    cx,
                )
            });
            _ = this.update_in(cx, |this, _, cx| {
                this.translation_data_clear_running = true;
                cx.notify();
            });

            let result = cx
                .background_spawn(async move {
                    service
                        .clear_translation_data()
                        .map_err(|error| format!("{error:#}"))
                })
                .await;
            let activity_notice = match &result {
                Ok(()) => ActivityNoticeEntry::new(
                    ActivityNoticeLevel::Success,
                    "Translation data cleared",
                    "Cached responses, generated PDFs, OCR results, and checkpoints were removed.",
                ),
                Err(error) => ActivityNoticeEntry::new(
                    ActivityNoticeLevel::Error,
                    "Could not clear translation data",
                    error.clone(),
                ),
            };
            activity_center.update(cx, |center, cx| {
                center.finish_task(task_id, activity_notice, cx);
            });
            _ = this.update_in(cx, |this, window, cx| {
                this.translation_data_clear_pending = false;
                this.translation_data_clear_running = false;
                match &result {
                    Ok(()) => {
                        window.push_notification(
                            Notification::success("Translation data cleared"),
                            cx,
                        );
                    }
                    Err(error) => {
                        window.push_notification(
                            Notification::error(error.clone())
                                .title("Could not clear translation data")
                                .autohide(false),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let config_path = cx.global::<SettingsStore>().config_path.clone();
        let directory = config_path
            .parent()
            .map(Path::to_path_buf)
            .filter(|path| path.exists())
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let dialog = prompt_for_new_path(
            window,
            cx,
            SavePathOptions::new(directory, crate::fl!("settings-export"), "config.kdl")
                .required_extension("kdl"),
        );

        cx.spawn(async move |this, cx| {
            let result = dialog.await;
            _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(Some(path))) => match cx.global::<SettingsStore>().export_to(&path) {
                        Ok(()) => {
                            this.save_error = None;
                            this.status = Some(
                                crate::fl!(
                                    "settings-exported-to",
                                    path = path.display().to_string()
                                )
                                .into(),
                            );
                        }
                        Err(error) => {
                            this.status = None;
                            this.save_error = Some(format!("{error:#}").into());
                        }
                    },
                    Ok(Err(error)) => {
                        this.status = None;
                        this.save_error = Some(format!("{error:#}").into());
                    }
                    Ok(Ok(None)) | Err(_) => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let config_path = cx.global::<SettingsStore>().config_path.clone();
        let directory = config_path
            .parent()
            .map(Path::to_path_buf)
            .filter(|path| path.exists())
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let dialog = prompt_for_paths(
            window,
            cx,
            OpenPathOptions::files(directory, crate::fl!("settings-import"))
                .filter(PathFilter::new("KDL", ["kdl"]))
                .allow_all_files(true),
        );
        self.status = None;
        self.save_error = None;
        cx.spawn_in(window, async move |this, cx| {
            let path = match dialog.await {
                Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                    Some(path) => path,
                    None => return,
                },
                Ok(Err(error)) => {
                    _ = this.update_in(cx, |this, _, cx| {
                        this.save_error = Some(
                            crate::fl!("settings-import-failed", error = format!("{error:#}"))
                                .into(),
                        );
                        cx.notify();
                    });
                    return;
                }
                Ok(Ok(None)) | Err(_) => return,
            };
            let read_path = path.clone();
            let parsed = cx
                .background_spawn(async move {
                    std::fs::read_to_string(&read_path)
                        .map_err(|error| format!("failed to read {}: {error}", read_path.display()))
                        .and_then(|contents| {
                            parse_settings(&contents).map_err(|error| format!("{error:#}"))
                        })
                })
                .await;
            _ = this.update_in(cx, |this, window, cx| match parsed {
                Ok(settings) => this.confirm_import(settings, path, window, cx),
                Err(error) => {
                    this.save_error =
                        Some(crate::fl!("settings-import-failed", error = error).into());
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn confirm_import(
        &mut self,
        settings: Settings,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = crate::fl!("settings-import-confirm-title");
        let detail = crate::fl!(
            "settings-import-confirm-detail",
            path = path.display().to_string()
        );
        let confirmation = window.prompt(
            PromptLevel::Warning,
            &title,
            Some(&detail),
            &[
                PromptButton::ok(crate::fl!("settings-import")),
                PromptButton::cancel(crate::fl!("cancel")),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if confirmation.await != Ok(0) {
                return;
            }
            _ = this.update_in(cx, |this, _, cx| {
                let before = cx.global::<SettingsStore>().settings.clone();
                let result =
                    SettingsStore::apply_imported_settings(settings, cx).map_err(|error| {
                        crate::fl!("settings-import-failed", error = format!("{error:#}"))
                    });
                if this.finish_settings_replacement(before, result, cx) {
                    this.status = Some(
                        crate::fl!("settings-imported-from", path = path.display().to_string())
                            .into(),
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn reset_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = crate::fl!("settings-reset-all-confirm-title");
        let detail = crate::fl!("settings-reset-all-confirm-detail");
        let confirmation = window.prompt(
            PromptLevel::Warning,
            &title,
            Some(&detail),
            &[
                PromptButton::ok(crate::fl!("settings-reset-all")),
                PromptButton::cancel(crate::fl!("cancel")),
            ],
            cx,
        );
        self.status = None;
        self.save_error = None;
        cx.spawn_in(window, async move |this, cx| {
            if confirmation.await != Ok(0) {
                return;
            }
            _ = this.update_in(cx, |this, _, cx| {
                let before = cx.global::<SettingsStore>().settings.clone();
                let result = SettingsStore::reset_all_settings(cx).map_err(|error| {
                    crate::fl!("settings-reset-all-failed", error = format!("{error:#}"))
                });
                if this.finish_settings_replacement(before, result, cx) {
                    if let Some(auth) = ChatGptSubscription::global(cx) {
                        auth.sign_out(cx);
                    }
                    this.status = Some(crate::fl!("settings-reset-all-complete").into());
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn close(&mut self, _: &PanelClose, window: &mut Window, _: &mut Context<Self>) {
        window.remove_window();
    }
}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui_font = setup_ui_font(window, cx);
        let window_settings = cx.global::<SettingsStore>().settings.window;
        if self.window_settings != window_settings {
            self.window_settings = window_settings;
            self.transparency_enabled = window_settings.transparency_enabled_by_default();
        }
        apply_window_background(window, window_settings, self.transparency_enabled);
        let shell = shell_background(
            cx.theme().colors,
            window_settings,
            self.transparency_enabled,
        );
        let settings = ComponentSettings::new("glyphst-reader-settings")
            .with_size(Size::Small)
            .with_group_variant(GroupBoxVariant::Outline)
            .default_selected_index(self.initial_selection)
            .pages(self.setting_pages(cx));
        let credential_warning = cx
            .global::<SettingsStore>()
            .credential_warning()
            .map(SharedString::from);
        let show_footer =
            self.status.is_some() || self.save_error.is_some() || credential_warning.is_some();
        let footer = show_footer.then(|| {
            div()
                .flex()
                .flex_col()
                .flex_none()
                .gap_2()
                .px_4()
                .py_3()
                .border_t_1()
                .border_color(cx.theme().border)
                .when_some(self.status.clone(), |this, status| {
                    this.child(div().text_color(cx.theme().success).child(status))
                })
                .when_some(self.save_error.clone(), |this, error| {
                    this.child(div().text_color(cx.theme().danger).child(error))
                })
                .when_some(credential_warning, |this, warning| {
                    this.child(div().text_color(cx.theme().danger).child(warning))
                })
        });
        let card = div()
            .id("settings-main-card")
            .debug_selector(|| "settings-main-card".to_string())
            .flex_1()
            .min_h_0()
            .mx(px(SURFACE_INSET))
            .mb(px(SURFACE_INSET))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .when(cx.theme().shadow, |this| this.shadow_sm())
            .child(div().flex_1().min_h_0().child(settings))
            .children(footer);
        div()
            .key_context("SettingsPanel")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(shell)
            .font(ui_font)
            .text_color(cx.theme().foreground)
            .text_size(ui_rems(13.0))
            .on_action(cx.listener(Self::close))
            .child(
                TitleBar::new().border_b(px(0.0)).bg(shell).child(
                    div()
                        .h_full()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_between()
                        .pr_2()
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .child(crate::fl!("settings-window-title")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(crate::fl!("escape-key")),
                        ),
                ),
            )
            .child(card)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CachedModelCatalog, ModelPickerState, OcrProbeError, SettingsWindow,
        test_ocr_connection_request,
    };
    use glyphst_ai::ai_client::AiModelInfo;
    use glyphst_settings::{
        AiApiStyle, AiModel, AiProvider, OcrSettings, Settings, SettingsStore, sync_component_theme,
    };
    use gpui::{
        AppContext as _, Modifiers, MouseButton, ScrollDelta, ScrollWheelEvent, TestAppContext,
        point, px, size,
    };
    use gpui_component::{Root, setting::SelectIndex};
    use std::{cell::RefCell, collections::BTreeSet, path::PathBuf, rc::Rc};

    #[test]
    fn ocr_probe_rejects_missing_default_provider_credentials() {
        assert!(matches!(
            test_ocr_connection_request(OcrSettings::default()),
            Err(OcrProbeError::InvalidAuth)
        ));
    }

    #[test]
    fn model_reference_keys_round_trip() {
        let parsed = SettingsWindow::parse_model_ref("openai::gpt-5").unwrap();
        assert_eq!(parsed.provider, AiProvider::OpenAi);
        assert_eq!(parsed.id, "gpt-5");
        assert_eq!(SettingsWindow::model_ref_key(&parsed), "openai::gpt-5");
        let custom = SettingsWindow::parse_model_ref(
            "custom-00000000-0000-4000-8000-000000000001::private-model",
        )
        .unwrap();
        assert!(custom.provider.is_custom());
        assert_eq!(
            SettingsWindow::model_ref_key(&custom),
            "custom-00000000-0000-4000-8000-000000000001::private-model"
        );
        assert!(SettingsWindow::parse_model_ref("missing-provider::model").is_none());
    }

    fn initialize_ui_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            glyphst_ui::activity::init(cx);
            let store = SettingsStore::memory();
            store.install_theme_catalog(cx).unwrap();
            cx.set_global(store);
            sync_component_theme(cx);
        });
    }

    fn enable_ui_test_persistence(cx: &mut TestAppContext, stem: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "glyphst-settings-ui-{stem}-{}-{:?}.kdl",
            std::process::id(),
            std::thread::current().id()
        ));
        cx.update(|cx| {
            cx.global_mut::<SettingsStore>().config_path = path.clone();
        });
        path
    }

    macro_rules! settings_page_render_test {
        ($name:ident, $page:expr, $group:expr) => {
            #[gpui::test]
            fn $name(cx: &mut TestAppContext) {
                initialize_ui_test(cx);
                let (_, cx) = cx.add_window_view(|window, cx| {
                    let settings = cx.new(|cx| {
                        let mut settings = SettingsWindow::new(|_| {}, cx);
                        settings.initial_selection = SelectIndex {
                            page_ix: $page,
                            group_ix: Some($group),
                        };
                        settings
                    });
                    Root::new(settings, window, cx)
                });
                cx.update(|window, cx| {
                    _ = window.draw(cx);
                });
            }
        };
    }

    settings_page_render_test!(renders_general_page, 0, 0);
    settings_page_render_test!(renders_keymap_page, 1, 0);
    settings_page_render_test!(renders_appearance_color_page, 2, 3);
    settings_page_render_test!(renders_ai_page, 3, 0);
    settings_page_render_test!(renders_provider_page, 4, 0);
    settings_page_render_test!(renders_translation_page, 5, 6);
    settings_page_render_test!(renders_ocr_page, 6, 1);

    #[gpui::test]
    fn general_page_renders_export_import_and_reset_actions(cx: &mut TestAppContext) {
        initialize_ui_test(cx);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = SettingsWindow::new(|_| {}, cx);
                settings.initial_selection = SelectIndex {
                    page_ix: 0,
                    group_ix: Some(9),
                };
                settings
            });
            Root::new(settings, window, cx)
        });
        cx.simulate_resize(size(px(1200.0), px(2400.0)));
        cx.update(|window, cx| {
            _ = window.draw(cx);
            _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("settings-export-button").is_some());
        assert!(cx.debug_bounds("settings-import-button").is_some());
        assert!(cx.debug_bounds("settings-reset-all-button").is_some());
    }

    #[gpui::test]
    fn import_settings_requires_confirmation_and_applies_valid_kdl(cx: &mut TestAppContext) {
        initialize_ui_test(cx);
        cx.update(|cx| {
            cx.global_mut::<SettingsStore>().settings.path_chooser =
                glyphst_settings::PathChooserBackend::System;
        });
        let persistence_path = enable_ui_test_persistence(cx, "import-confirmation");
        let import_path = persistence_path.with_extension("import");
        let imported = Settings {
            behavior: glyphst_settings::BehaviorSettings {
                default_zoom: 1.5,
                ..glyphst_settings::BehaviorSettings::default()
            },
            ..Settings::default()
        };
        std::fs::write(&import_path, imported.to_kdl_string()).unwrap();
        let settings_view = Rc::new(RefCell::new(None));
        let captured_settings_view = settings_view.clone();
        let (_, cx) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| SettingsWindow::new(|_| {}, cx));
            *captured_settings_view.borrow_mut() = Some(settings.clone());
            Root::new(settings, window, cx)
        });
        let settings = settings_view.borrow().as_ref().unwrap().clone();

        cx.update(|window, cx| {
            settings.update(cx, |this, cx| this.import(window, cx));
        });
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response({
            let import_path = import_path.clone();
            move |options| {
                assert!(options.files);
                assert!(!options.directories);
                assert!(!options.multiple);
                Some(vec![import_path])
            }
        });
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer(&crate::fl!("cancel"));
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert_eq!(
                cx.global::<SettingsStore>().settings.behavior.default_zoom,
                Settings::default().behavior.default_zoom
            );
        });

        cx.update(|window, cx| {
            settings.update(cx, |this, cx| this.import(window, cx));
        });
        cx.simulate_path_prompt_response({
            let import_path = import_path.clone();
            move |_| Some(vec![import_path])
        });
        cx.run_until_parked();
        cx.simulate_prompt_answer(&crate::fl!("settings-import"));
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert_eq!(
                cx.global::<SettingsStore>().settings.behavior.default_zoom,
                1.5
            );
            assert!(settings.read(cx).status.is_some());
            assert!(settings.read(cx).save_error.is_none());
        });

        _ = std::fs::remove_file(import_path);
        _ = std::fs::remove_file(persistence_path);
    }

    #[gpui::test]
    fn invalid_import_keeps_current_settings_and_reports_an_error(cx: &mut TestAppContext) {
        initialize_ui_test(cx);
        cx.update(|cx| {
            cx.global_mut::<SettingsStore>().settings.path_chooser =
                glyphst_settings::PathChooserBackend::System;
        });
        let persistence_path = enable_ui_test_persistence(cx, "invalid-import");
        let import_path = persistence_path.with_extension("invalid");
        std::fs::write(&import_path, "settings { definitely not valid KDL").unwrap();
        let settings_view = Rc::new(RefCell::new(None));
        let captured_settings_view = settings_view.clone();
        let (_, cx) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| SettingsWindow::new(|_| {}, cx));
            *captured_settings_view.borrow_mut() = Some(settings.clone());
            Root::new(settings, window, cx)
        });
        let settings = settings_view.borrow().as_ref().unwrap().clone();

        cx.update(|window, cx| {
            settings.update(cx, |this, cx| this.import(window, cx));
        });
        cx.simulate_path_prompt_response({
            let import_path = import_path.clone();
            move |_| Some(vec![import_path])
        });
        cx.run_until_parked();

        assert!(!cx.has_pending_prompt());
        cx.update(|_, cx| {
            let expected = Settings {
                path_chooser: glyphst_settings::PathChooserBackend::System,
                ..Settings::default()
            };
            assert_eq!(cx.global::<SettingsStore>().settings, expected);
            assert!(settings.read(cx).status.is_none());
            assert!(settings.read(cx).save_error.is_some());
        });

        _ = std::fs::remove_file(import_path);
        _ = std::fs::remove_file(persistence_path);
    }

    #[gpui::test]
    fn reset_all_settings_requires_confirmation(cx: &mut TestAppContext) {
        initialize_ui_test(cx);
        let persistence_path = enable_ui_test_persistence(cx, "reset-confirmation");
        cx.update(|cx| {
            let mut settings = Settings::default();
            settings.behavior.default_zoom = 1.5;
            cx.global_mut::<SettingsStore>()
                .set_settings_in_memory(settings);
        });
        let settings_view = Rc::new(RefCell::new(None));
        let captured_settings_view = settings_view.clone();
        let (_, cx) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| SettingsWindow::new(|_| {}, cx));
            *captured_settings_view.borrow_mut() = Some(settings.clone());
            Root::new(settings, window, cx)
        });
        let settings = settings_view.borrow().as_ref().unwrap().clone();

        cx.update(|window, cx| {
            settings.update(cx, |this, cx| this.reset_all(window, cx));
        });
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer(&crate::fl!("cancel"));
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert_eq!(
                cx.global::<SettingsStore>().settings.behavior.default_zoom,
                1.5
            );
        });

        cx.update(|window, cx| {
            settings.update(cx, |this, cx| this.reset_all(window, cx));
        });
        cx.simulate_prompt_answer(&crate::fl!("settings-reset-all"));
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert_eq!(cx.global::<SettingsStore>().settings, Settings::default());
            assert!(settings.read(cx).status.is_some());
            assert!(settings.read(cx).save_error.is_none());
        });

        _ = std::fs::remove_file(persistence_path);
    }

    #[gpui::test]
    fn settings_window_does_not_render_background_activity(cx: &mut TestAppContext) {
        initialize_ui_test(cx);
        let center = cx.update(|cx| glyphst_ui::activity::ActivityCenter::global(cx));
        center.update(cx, |center, cx| {
            center.add_task(
                glyphst_ui::activity::BackgroundTaskEntry::new(
                    "Translate a-long-document-name.pdf",
                    "Translating",
                ),
                cx,
            );
        });
        let (_, cx) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| SettingsWindow::new(|_| {}, cx));
            Root::new(settings, window, cx)
        });

        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("background-activity").is_none());
    }

    #[gpui::test]
    fn settings_window_renders_an_inset_main_surface(cx: &mut TestAppContext) {
        initialize_ui_test(cx);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| SettingsWindow::new(|_| {}, cx));
            Root::new(settings, window, cx)
        });

        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("settings-main-card").is_some());
    }

    #[gpui::test]
    fn model_picker_stages_manual_ids_without_fetching_on_open(cx: &mut TestAppContext) {
        initialize_ui_test(cx);
        let persistence_path = enable_ui_test_persistence(cx, "manual-model");
        let settings_view = Rc::new(RefCell::new(None));
        let captured_settings_view = settings_view.clone();
        let (_, cx) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = SettingsWindow::new(|_| {}, cx);
                settings.initial_selection = SelectIndex {
                    page_ix: 4,
                    group_ix: Some(1),
                };
                settings
            });
            *captured_settings_view.borrow_mut() = Some(settings.clone());
            Root::new(settings, window, cx)
        });
        let settings = settings_view
            .borrow()
            .as_ref()
            .expect("settings view")
            .clone();
        let provider = AiProvider::OpenAi;

        cx.update(|window, cx| {
            settings.update(cx, |this, cx| {
                this.set_model_picker_open(provider, true, cx);
                assert!(this.model_catalog_operation.provider.is_none());
                assert!(!this.model_catalogs.contains_key(&provider));
                assert!(this.stage_manual_model_id(provider, "manual-model".to_string()));
                assert!(!this.stage_manual_model_id(provider, "manual-model".to_string()));
            });
            _ = window.draw(cx);
            _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("provider-manual-model-openai-manual-model")
                .is_some()
        );
        cx.update(|_, cx| {
            assert!(
                cx.global::<SettingsStore>()
                    .settings
                    .ai
                    .provider_config(provider)
                    .model("manual-model")
                    .is_none()
            );
            settings.update(cx, |this, cx| {
                this.set_model_picker_open(provider, false, cx);
                assert!(this.model_picker.manual.is_empty());
            });
            assert!(settings.read(cx).model_picker.selected.is_empty());
            settings.update(cx, |this, cx| {
                this.set_model_picker_open(provider, true, cx);
                assert!(this.stage_manual_model_id(provider, "manual-model".to_string()));
                this.add_selected_models(provider, cx);
            });
            assert!(
                cx.global::<SettingsStore>()
                    .settings
                    .ai
                    .provider_config(provider)
                    .model("manual-model")
                    .is_some()
            );
        });
        _ = std::fs::remove_file(persistence_path);
    }

    #[gpui::test]
    fn renders_custom_provider_rows_and_models(cx: &mut TestAppContext) {
        initialize_ui_test(cx);
        let provider = cx.update(|cx| {
            let mut settings = cx.global::<SettingsStore>().settings.clone();
            let provider = settings.ai.add_custom_provider(
                "Local Models",
                AiApiStyle::OpenAi,
                "http://127.0.0.1:1234/v1",
                "",
            );
            settings
                .ai
                .provider_config_mut(provider)
                .ensure_model(AiModel::new("local-model", "Local Model", "Local Models"));
            settings.ai.provider_order = vec![provider];
            cx.global_mut::<SettingsStore>()
                .set_settings_in_memory(settings);
            provider
        });
        let settings_view = Rc::new(RefCell::new(None));
        let captured_settings_view = settings_view.clone();
        let (_, cx) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = SettingsWindow::new(|_| {}, cx);
                settings.initial_selection = SelectIndex {
                    page_ix: 4,
                    group_ix: Some(1),
                };
                settings
            });
            *captured_settings_view.borrow_mut() = Some(settings.clone());
            Root::new(settings, window, cx)
        });
        cx.simulate_resize(size(px(2000.0), px(1200.0)));
        cx.update(|window, cx| {
            _ = window.draw(cx);
            _ = window.draw(cx);
        });

        let host = provider.as_str();
        let api_host_selector: &'static str =
            Box::leak(format!("setting-bound-input-provider-api-host-{host}").into_boxed_str());
        let api_key_selector: &'static str =
            Box::leak(format!("setting-bound-input-provider-api-key-{host}").into_boxed_str());
        let compatibility_divider_selector: &'static str =
            Box::leak(format!("provider-divider-{host}-compatibility").into_boxed_str());
        let credential_divider_selector: &'static str =
            Box::leak(format!("provider-divider-{host}-credentials").into_boxed_str());
        let chips_selector: &'static str =
            Box::leak(format!("provider-model-chips-{host}").into_boxed_str());
        let chip_selector: &'static str =
            Box::leak(format!("provider-model-chip-{host}-local-model").into_boxed_str());
        let trash_selector: &'static str =
            Box::leak(format!("provider-model-trash-{host}").into_boxed_str());
        let header_selector: &'static str =
            Box::leak(format!("provider-model-header-{host}").into_boxed_str());
        let add_models_selector: &'static str =
            Box::leak(format!("provider-add-models-{host}").into_boxed_str());
        cx.debug_bounds(api_host_selector)
            .expect("API Host input host");
        cx.debug_bounds(api_key_selector)
            .expect("API Key input host");
        let compatibility_divider = cx
            .debug_bounds(compatibility_divider_selector)
            .expect("Compatibility divider");
        let credential_divider = cx
            .debug_bounds(credential_divider_selector)
            .expect("Credential divider");
        for divider in [compatibility_divider, credential_divider] {
            assert_eq!(divider.size.height, px(1.0));
            assert!(divider.size.width > px(400.0));
        }
        let header = cx.debug_bounds(header_selector).expect("Model List header");
        let add_models = cx
            .debug_bounds(add_models_selector)
            .expect("Add Models button");
        assert_eq!(add_models.right(), header.right());
        assert!(add_models.left() > header.left());

        assert!(cx.debug_bounds(trash_selector).is_none());
        let settings = settings_view
            .borrow()
            .as_ref()
            .expect("settings view")
            .clone();
        let initial_chip = cx
            .debug_bounds(chip_selector)
            .expect("initial draggable model chip");
        cx.simulate_event(ScrollWheelEvent {
            position: initial_chip.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-160.0))),
            ..Default::default()
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        let chip = cx
            .debug_bounds(chip_selector)
            .expect("draggable model chip");
        cx.simulate_mouse_down(chip.center(), MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            point(chip.center().x + px(8.0), chip.center().y),
            Some(MouseButton::Left),
            Modifiers::none(),
        );
        cx.update(|window, cx| {
            _ = window.draw(cx);
            _ = window.draw(cx);
        });
        assert_eq!(
            settings.read_with(cx, |this, _| this.active_model_drag),
            Some(provider)
        );
        let chips = cx.debug_bounds(chips_selector).expect("model chips");
        let trash = cx.debug_bounds(trash_selector).expect("model trash target");
        assert_eq!(trash.size.height, px(48.0));
        assert!(trash.size.width > px(400.0));
        assert!(trash.top() >= chips.bottom());

        cx.simulate_mouse_move(trash.center(), Some(MouseButton::Left), Modifiers::none());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        cx.simulate_mouse_up(trash.center(), MouseButton::Left, Modifiers::none());
        cx.update(|window, cx| {
            _ = window.draw(cx);
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds(trash_selector).is_none());
    }

    #[gpui::test]
    fn chatgpt_subscription_models_use_shared_add_and_trash_controls(cx: &mut TestAppContext) {
        initialize_ui_test(cx);
        let persistence_path = enable_ui_test_persistence(cx, "subscription-model");
        cx.update(|cx| {
            let mut settings = cx.global::<SettingsStore>().settings.clone();
            settings.ai.provider_order = vec![AiProvider::ChatGptSubscription];
            settings
                .ai
                .provider_config_mut(AiProvider::ChatGptSubscription)
                .enabled = true;
            cx.global_mut::<SettingsStore>()
                .set_settings_in_memory(settings);
        });
        let (_, cx) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = SettingsWindow::new(|_| {}, cx);
                settings.initial_selection = SelectIndex {
                    page_ix: 4,
                    group_ix: Some(1),
                };
                settings
            });
            Root::new(settings, window, cx)
        });
        cx.simulate_resize(size(px(2000.0), px(1200.0)));
        cx.update(|window, cx| {
            _ = window.draw(cx);
            _ = window.draw(cx);
        });

        assert!(
            cx.debug_bounds("provider-add-models-openai-subscribed")
                .is_some()
        );
        let chip = cx
            .debug_bounds("provider-model-chip-openai-subscribed-gpt-5.6-sol")
            .expect("Subscription model chip");
        cx.simulate_mouse_down(chip.center(), MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            point(chip.center().x + px(8.0), chip.center().y),
            Some(MouseButton::Left),
            Modifiers::none(),
        );
        cx.update(|window, cx| {
            _ = window.draw(cx);
            _ = window.draw(cx);
        });
        let trash = cx
            .debug_bounds("provider-model-trash-openai-subscribed")
            .expect("Subscription model trash target");
        cx.simulate_mouse_move(trash.center(), Some(MouseButton::Left), Modifiers::none());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        cx.simulate_mouse_up(trash.center(), MouseButton::Left, Modifiers::none());
        cx.update(|window, cx| {
            _ = window.draw(cx);
            _ = window.draw(cx);
            assert!(
                cx.global::<SettingsStore>()
                    .settings
                    .ai
                    .provider_config(AiProvider::ChatGptSubscription)
                    .model("gpt-5.6-sol")
                    .is_none()
            );
        });
        _ = std::fs::remove_file(persistence_path);
    }

    #[gpui::test]
    fn renders_cached_provider_model_picker(cx: &mut TestAppContext) {
        initialize_ui_test(cx);
        cx.update(|cx| {
            let mut settings = cx.global::<SettingsStore>().settings.clone();
            settings.ai.provider_order = vec![AiProvider::OpenAi];
            cx.global_mut::<SettingsStore>()
                .set_settings_in_memory(settings);
        });
        let (_, cx) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = SettingsWindow::new(|_| {}, cx);
                settings.initial_selection = SelectIndex {
                    page_ix: 4,
                    group_ix: Some(1),
                };
                let provider = AiProvider::OpenAi;
                let signature = SettingsWindow::current_model_request_signature(provider, cx);
                settings.model_catalogs.insert(
                    provider,
                    CachedModelCatalog {
                        signature,
                        models: (0..120)
                            .map(|ix| AiModelInfo {
                                id: format!("remote-model-{ix:03}"),
                                label: format!("Remote Model {ix:03}"),
                                owned_by: Some("test".to_string()),
                            })
                            .collect(),
                    },
                );
                settings.model_picker = ModelPickerState {
                    provider: Some(provider),
                    selected: BTreeSet::from(["remote-model-000".to_string()]),
                    manual: BTreeSet::new(),
                };
                settings
            });
            Root::new(settings, window, cx)
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
            _ = window.draw(cx);
        });

        let panel = cx
            .debug_bounds("provider-model-picker-panel")
            .expect("model picker panel");
        let catalog = cx
            .debug_bounds("provider-model-catalog-scroll")
            .expect("model catalog scroll viewport");
        let footer = cx
            .debug_bounds("provider-model-picker-footer")
            .expect("model picker footer");
        let last_selector = "provider-model-catalog-option-openai-remote-model-119";
        let last_before = cx
            .debug_bounds(last_selector)
            .expect("last catalog model")
            .top();
        assert!(panel.size.height <= px(520.0));
        assert!(
            catalog.size.height < panel.size.height,
            "panel={panel:?}, catalog={catalog:?}, footer={footer:?}"
        );
        assert!(footer.bottom() <= panel.bottom());

        cx.simulate_event(ScrollWheelEvent {
            position: catalog.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-240.0))),
            ..Default::default()
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
            _ = window.draw(cx);
        });
        let last_after = cx
            .debug_bounds(last_selector)
            .expect("last catalog model after scrolling")
            .top();
        let footer_after = cx
            .debug_bounds("provider-model-picker-footer")
            .expect("model picker footer after scrolling");
        assert!(
            last_after < last_before,
            "panel={panel:?}, catalog={catalog:?}, footer={footer:?}, \
             last_before={last_before:?}, last_after={last_after:?}"
        );
        assert_eq!(footer_after, footer);
    }
}
