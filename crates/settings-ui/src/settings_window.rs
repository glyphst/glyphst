mod fields;
mod pages;

use glyphst_actions::settings_panel::PanelClose;
use glyphst_ai::ai_client::{AiModelInfo, list_ai_models, test_ai_connection};
use glyphst_keymap::keybindings;
use glyphst_pdf_translation_app::{
    CancellationToken, PaddleClient, PaddleClientConfig, PaddleErrorCategory,
    PdfTranslationService, UnlimitedClient, UnlimitedClientConfig, UnlimitedErrorCategory,
};
use glyphst_settings::{
    AiModel, AiProvider, AiThinkingLevel, ModelRef, OcrProvider, OcrSettings, ResolvedAiConfig,
    Settings, SettingsStore, localize, setup_ui_font, sync_component_theme, ui_rems,
};
use glyphst_ui::{
    notification_center::{
        BackgroundTaskEntry, NotificationCenter, NotificationEntry, NotificationLevel,
        SUCCESS_NOTICE_DURATION,
    },
    root::{RootWindowHandle, render_overlays},
};
use gpui::{
    App, AppContext, Bounds, Context, Entity, FocusHandle, Focusable, IntoElement, PromptButton,
    PromptLevel, Render, SharedString, Task, Window, WindowBounds, WindowOptions, div, prelude::*,
    px, size,
};
use gpui_component::{
    ActiveTheme as _, Root, Sizable as _, Size, TitleBar,
    button::Button,
    group_box::GroupBoxVariant,
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

pub fn open_settings_window(open_keymap: fn(&mut App), cx: &mut App) {
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
            app_id: Some("glyphst".to_string()),
            window_min_size: Some(size(px(720.0), px(420.0))),
            ..Default::default()
        },
        move |window, cx| {
            window.set_app_id("glyphst");
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

enum OcrProbeError {
    InvalidAuth,
    Retryable,
    Provider(Option<String>),
}

fn test_ocr_connection_request(settings: OcrSettings) -> Result<(), OcrProbeError> {
    let cancellation = CancellationToken::new();
    match settings.provider {
        OcrProvider::Paddle => PaddleClient::new(
            PaddleClientConfig {
                base_url: settings.paddle.endpoint,
                request_timeout: Duration::from_secs(15),
                ..PaddleClientConfig::default()
            },
            settings.paddle.token,
        )
        .map_err(|_| OcrProbeError::InvalidAuth)?
        .probe(&cancellation)
        .map(|_| ())
        .map_err(|error| match error.category {
            PaddleErrorCategory::Unauthorized | PaddleErrorCategory::PermissionDenied => {
                OcrProbeError::InvalidAuth
            }
            PaddleErrorCategory::QueueFull
            | PaddleErrorCategory::ServiceUnavailable
            | PaddleErrorCategory::PollTimeout => OcrProbeError::Retryable,
            _ => OcrProbeError::Provider(error.provider_code),
        }),
        OcrProvider::Unlimited => UnlimitedClient::new(
            UnlimitedClientConfig {
                base_url: settings.unlimited.endpoint,
                request_timeout: Duration::from_secs(15),
                download_timeout: Duration::from_secs(15),
                ..UnlimitedClientConfig::default()
            },
            settings.unlimited.api_key,
            settings.unlimited.secret_key,
        )
        .map_err(|_| OcrProbeError::InvalidAuth)?
        .probe(&cancellation)
        .map(|_| ())
        .map_err(|error| match error.category {
            UnlimitedErrorCategory::Unauthorized | UnlimitedErrorCategory::PermissionDenied => {
                OcrProbeError::InvalidAuth
            }
            UnlimitedErrorCategory::QueueFull
            | UnlimitedErrorCategory::ServiceUnavailable
            | UnlimitedErrorCategory::PollTimeout => OcrProbeError::Retryable,
            _ => OcrProbeError::Provider(error.provider_code.map(|code| code.to_string())),
        }),
    }
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
    pub(super) ocr_operation: Option<OcrOperation>,
    _ocr_task: Option<Task<()>>,
    pub(super) translation_data_clear_pending: bool,
    pub(super) translation_data_clear_running: bool,
}

impl SettingsWindow {
    pub fn new(open_keymap: fn(&mut App), cx: &mut Context<Self>) -> Self {
        let font_settings = &cx.global::<SettingsStore>().settings.font;
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
            ocr_operation: None,
            _ocr_task: None,
            translation_data_clear_pending: false,
            translation_data_clear_running: false,
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
        let provider = AiProvider::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == provider)?;
        (!id.trim().is_empty()).then(|| ModelRef::new(provider, id))
    }

    pub(super) fn model_options(settings: &Settings) -> Vec<(SharedString, SharedString)> {
        let mut options = Vec::new();
        for provider in AiProvider::ALL {
            let config = settings.ai.provider_config(provider);
            if !config.enabled {
                continue;
            }
            options.extend(config.models.into_iter().map(|model| {
                Self::setting_option(
                    Self::model_ref_key(&ModelRef::new(provider, model.id)),
                    format!("{} — {}", provider.label(), model.name),
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
        let config = settings.ai.provider_config(provider);
        let model = config
            .models
            .first()
            .map(|model| model.id.clone())
            .unwrap_or_else(|| provider.default_model().to_string());
        let base_url_overridden = config.base_url.is_some();
        ResolvedAiConfig {
            provider,
            model,
            api_key: config.api_key,
            base_url: config
                .base_url
                .unwrap_or_else(|| provider.default_base_url().to_string()),
            base_url_overridden,
            thinking_level: AiThinkingLevel::Off,
        }
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
        };

        let signature = Self::current_model_request_signature(provider, cx);
        let has_current_cache = self
            .model_catalogs
            .get(&provider)
            .is_some_and(|catalog| catalog.signature == signature);
        let fetch_in_progress = self.model_catalog_operation.provider == Some(provider)
            && self.model_catalog_operation.loading
            && self.model_catalog_operation.signature.as_ref() == Some(&signature);
        if !has_current_cache {
            self.model_catalogs.remove(&provider);
            if !fetch_in_progress {
                self.fetch_provider_models(provider, cx);
            } else {
                cx.notify();
            }
        } else {
            cx.notify();
        }
    }

    pub(super) fn fetch_provider_models(&mut self, provider: AiProvider, cx: &mut Context<Self>) {
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
        if self.model_picker.selected.contains(&model_id) {
            self.model_picker.selected.remove(&model_id);
        } else {
            self.model_picker.selected.insert(model_id);
        }
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
            self.model_picker.selected.clear();
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
        let discovered = self
            .model_catalogs
            .get(&provider)
            .map(|catalog| catalog.models.clone())
            .unwrap_or_default();
        if selected.is_empty() {
            return;
        }

        let mut settings = cx.global::<SettingsStore>().settings.clone();
        let config = settings.ai.provider_config_mut(provider);
        let mut added = 0usize;
        for model in discovered {
            if !selected.contains(&model.id) {
                continue;
            }
            let group = model
                .owned_by
                .unwrap_or_else(|| provider.label().to_string());
            let old_len = config.models.len();
            config.ensure_model(AiModel::new(model.id, model.label, group));
            added += usize::from(config.models.len() != old_len);
        }
        self.persist_settings(settings, cx);
        self.status = Some(crate::fl!("settings-added-models", count = added as i64).into());
        cx.notify();
    }

    pub(super) fn test_ocr_connection(&mut self, provider: OcrProvider, cx: &mut Context<Self>) {
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
        if !cx.has_global::<PdfTranslationService>() || NotificationCenter::try_global(cx).is_none()
        {
            self.status = None;
            self.save_error = Some("Translation data service is unavailable.".into());
            cx.notify();
            return;
        }

        let service = cx.global::<PdfTranslationService>().clone();
        let notification_center = NotificationCenter::global(cx);
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
        cx.spawn(async move |this, cx| {
            if confirmation.await != Ok(0) {
                _ = this.update(cx, |this, cx| {
                    this.translation_data_clear_pending = false;
                    this.translation_data_clear_running = false;
                    cx.notify();
                });
                return;
            }

            let task_id = notification_center.update(cx, |center, cx| {
                center.add_task(
                    BackgroundTaskEntry::new(
                        "Clearing translation data",
                        "Removing cached responses, generated PDFs, OCR results, and checkpoints.",
                    ),
                    cx,
                )
            });
            _ = this.update(cx, |this, cx| {
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
            notification_center.update(cx, |center, cx| {
                center.remove_task(task_id, cx);
                match &result {
                    Ok(()) => {
                        center.show_notification(
                            NotificationEntry::new(
                                NotificationLevel::Success,
                                "Translation data cleared",
                            ),
                            Some(SUCCESS_NOTICE_DURATION),
                            cx,
                        );
                    }
                    Err(error) => {
                        let mut notification = NotificationEntry::new(
                            NotificationLevel::Error,
                            "Could not clear translation data",
                        );
                        notification.detail = Some(error.clone().into());
                        center.show_notification(notification, None, cx);
                    }
                }
            });
            _ = this.update(cx, |this, cx| {
                this.translation_data_clear_pending = false;
                this.translation_data_clear_running = false;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn export(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let config_path = cx.global::<SettingsStore>().config_path.clone();
        let directory = config_path
            .parent()
            .map(Path::to_path_buf)
            .filter(|path| path.exists())
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let dialog = cx.prompt_for_new_path(&directory, Some("config.kdl"));

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
        let root_overlays = render_overlays(window, cx);
        let ui_font = setup_ui_font(window, cx);
        let resolved = cx.global::<SettingsStore>().resolved_theme;
        let settings = ComponentSettings::new("glyphst-settings")
            .with_size(Size::Small)
            .with_group_variant(GroupBoxVariant::Outline)
            .default_selected_index(self.initial_selection)
            .pages(self.setting_pages(cx));
        let background_activity = NotificationCenter::try_global(cx).and_then(|center| {
            center.update(cx, |center, cx| center.render_activity(resolved, cx))
        });
        let center_notifications = window
            .is_window_active()
            .then(|| {
                NotificationCenter::try_global(cx).and_then(|center| {
                    center.update(cx, |center, cx| center.render_notifications(resolved, cx))
                })
            })
            .flatten();

        div()
            .key_context("SettingsPanel")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(cx.theme().background)
            .font(ui_font)
            .text_color(cx.theme().foreground)
            .text_size(ui_rems(13.0))
            .on_action(cx.listener(Self::close))
            .child(
                TitleBar::new().child(
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
            .child(div().flex_1().min_h_0().child(settings))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(div().when_some(background_activity, |this, activity| {
                                this.child(activity)
                            }))
                            .child(
                                Button::new("settings-export")
                                    .label(crate::fl!("export"))
                                    .outline()
                                    .small()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.export(window, cx);
                                    })),
                            ),
                    )
                    .when_some(self.status.clone(), |this, status| {
                        this.child(div().text_color(cx.theme().success).child(status))
                    })
                    .when_some(self.save_error.clone(), |this, error| {
                        this.child(div().text_color(cx.theme().danger).child(error))
                    }),
            )
            .when_some(center_notifications, |this, notifications| {
                this.child(
                    div()
                        .absolute()
                        .right_3()
                        .bottom(px(72.0))
                        .child(notifications),
                )
            })
            .children(root_overlays)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CachedModelCatalog, ModelPickerState, OcrProbeError, SettingsWindow,
        test_ocr_connection_request,
    };
    use glyphst_ai::ai_client::AiModelInfo;
    use glyphst_settings::{AiProvider, OcrSettings, SettingsStore, sync_component_theme};
    use gpui::{AppContext as _, TestAppContext};
    use gpui_component::{Root, setting::SelectIndex};
    use std::collections::BTreeSet;

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
        assert!(SettingsWindow::parse_model_ref("missing-provider::model").is_none());
    }

    fn initialize_ui_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            glyphst_ui::notification_center::init(cx);
            let store = SettingsStore::memory();
            store.install_theme_catalog(cx).unwrap();
            cx.set_global(store);
            sync_component_theme(cx);
        });
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
    fn renders_cached_provider_model_picker(cx: &mut TestAppContext) {
        initialize_ui_test(cx);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = SettingsWindow::new(|_| {}, cx);
                settings.initial_selection = SelectIndex {
                    page_ix: 4,
                    group_ix: Some(0),
                };
                let provider = AiProvider::OpenAi;
                let signature = SettingsWindow::current_model_request_signature(provider, cx);
                settings.model_catalogs.insert(
                    provider,
                    CachedModelCatalog {
                        signature,
                        models: vec![
                            AiModelInfo {
                                id: "remote-model-a".to_string(),
                                label: "Remote Model A".to_string(),
                                owned_by: Some("test".to_string()),
                            },
                            AiModelInfo {
                                id: "remote-model-b".to_string(),
                                label: "Remote Model B".to_string(),
                                owned_by: Some("test".to_string()),
                            },
                        ],
                    },
                );
                settings.model_picker = ModelPickerState {
                    provider: Some(provider),
                    selected: BTreeSet::from(["remote-model-a".to_string()]),
                };
                settings
            });
            Root::new(settings, window, cx)
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
            _ = window.draw(cx);
        });
    }
}
