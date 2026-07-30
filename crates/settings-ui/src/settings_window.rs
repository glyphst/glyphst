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
    activity::{ActivityCenter, BackgroundTaskEntry},
    root::{RootWindowHandle, render_overlays},
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
    pub(super) provider_rename: Option<ProviderRenameState>,
    pub(super) active_model_drag: Option<AiProvider>,
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
            provider_rename: None,
            active_model_drag: None,
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
        let provider = AiProvider::from_str(provider)?;
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
        let base_url_overridden = provider.is_custom() && config.base_url.is_some();
        ResolvedAiConfig {
            provider,
            provider_name: config.name,
            api_style: config.api_style,
            enabled: true,
            model,
            api_key: config.api_key,
            base_url: if provider.is_custom() {
                config.base_url.unwrap_or_default()
            } else {
                provider.default_base_url().to_string()
            },
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
        }
        if fetch_in_progress {
            cx.notify();
        } else {
            self.fetch_provider_models(provider, cx);
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
            let group = model.owned_by.unwrap_or_else(|| config.name.clone());
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
        if !cx.has_global::<PdfTranslationService>() || ActivityCenter::try_global(cx).is_none() {
            self.status = None;
            self.save_error = Some("Translation data service is unavailable.".into());
            cx.notify();
            return;
        }

        let service = cx.global::<PdfTranslationService>().clone();
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
            activity_center.update(cx, |center, cx| {
                center.remove_task(task_id, cx);
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
        let settings = ComponentSettings::new("glyphst-reader-settings")
            .with_size(Size::Small)
            .with_group_variant(GroupBoxVariant::Outline)
            .default_selected_index(self.initial_selection)
            .pages(self.setting_pages(cx));
        let background_activity =
            ActivityCenter::try_global(cx).and_then(|center| center.read(cx).render_activity());
        let show_footer =
            background_activity.is_some() || self.status.is_some() || self.save_error.is_some();
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
            .when(show_footer, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .gap_2()
                        .px_4()
                        .py_3()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .when_some(background_activity, |this, activity| {
                            this.child(div().flex().items_center().child(activity))
                        })
                        .when_some(self.status.clone(), |this, status| {
                            this.child(div().text_color(cx.theme().success).child(status))
                        })
                        .when_some(self.save_error.clone(), |this, error| {
                            this.child(div().text_color(cx.theme().danger).child(error))
                        }),
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
    use glyphst_settings::{
        AiApiStyle, AiModel, AiProvider, ModelRef, OcrSettings, SettingsStore, sync_component_theme,
    };
    use gpui::{
        AppContext as _, Modifiers, MouseButton, ScrollDelta, ScrollWheelEvent, TestAppContext,
        point, px, size,
    };
    use gpui_component::{Root, setting::SelectIndex};
    use std::{cell::RefCell, collections::BTreeSet, rc::Rc};

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
    fn microsoft_translator_text_fields_share_a_bounded_control_column(cx: &mut TestAppContext) {
        initialize_ui_test(cx);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = SettingsWindow::new(|_| {}, cx);
                settings.initial_selection = SelectIndex {
                    page_ix: 5,
                    group_ix: Some(4),
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

        let api_key_row = cx.debug_bounds("setting-item-5-4-1").unwrap();
        let api_key_label = cx.debug_bounds("setting-item-label-5-4-1").unwrap();
        let api_key_field = cx.debug_bounds("setting-item-field-5-4-1").unwrap();
        let api_key_input = cx
            .debug_bounds("setting-bound-input-translation-api-key-microsoft")
            .unwrap();
        let endpoint_row = cx.debug_bounds("setting-item-5-4-2").unwrap();
        let endpoint_label = cx.debug_bounds("setting-item-label-5-4-2").unwrap();
        let endpoint_field = cx.debug_bounds("setting-item-field-5-4-2").unwrap();
        let endpoint_input = cx
            .debug_bounds("setting-bound-input-translation-base-url-microsoft")
            .unwrap();
        let region_row = cx.debug_bounds("setting-item-5-4-3").unwrap();
        let region_label = cx.debug_bounds("setting-item-label-5-4-3").unwrap();
        let region_field = cx.debug_bounds("setting-item-field-5-4-3").unwrap();
        let region_input = cx
            .debug_bounds("setting-bound-input-translation-microsoft-region")
            .unwrap();

        for (row, label, field, input) in [
            (api_key_row, api_key_label, api_key_field, api_key_input),
            (region_row, region_label, region_field, region_input),
        ] {
            assert_eq!(field.right(), row.right());
            assert_eq!(field.size.width, px(480.0));
            assert_eq!(input, field);
            assert!(field.left() > label.right());
        }
        assert_eq!(api_key_field.left(), region_field.left());
        assert_eq!(api_key_field.right(), region_field.right());

        assert_eq!(endpoint_field.right(), endpoint_row.right());
        assert_eq!(endpoint_field.size.width, px(480.0));
        assert_eq!(endpoint_input, endpoint_field);
        assert_eq!(endpoint_field.left(), api_key_field.left());
        assert!(endpoint_field.top() > endpoint_label.bottom());
    }

    #[gpui::test]
    fn long_model_selector_uses_the_control_column_without_stretching_short_selectors(
        cx: &mut TestAppContext,
    ) {
        initialize_ui_test(cx);
        cx.update(|cx| {
            let mut settings = cx.global::<SettingsStore>().settings.clone();
            let model_id = "long-translation-model";
            let config = settings.ai.provider_config_mut(AiProvider::OpenAi);
            config.enabled = true;
            config.ensure_model(AiModel::new(
                model_id,
                "Long translation model name ".repeat(80),
                "OpenAI",
            ));
            settings.ai.provider_order = vec![AiProvider::OpenAi];
            settings.ai.translation_model = Some(ModelRef::new(AiProvider::OpenAi, model_id));
            cx.global_mut::<SettingsStore>()
                .set_settings_in_memory(settings);
        });
        let (_, cx) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = SettingsWindow::new(|_| {}, cx);
                settings.initial_selection = SelectIndex {
                    page_ix: 3,
                    group_ix: Some(0),
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

        let model_field = cx.debug_bounds("setting-item-field-3-0-2").unwrap();
        let model_selector = cx.debug_bounds("setting-dropdown-3-0-2").unwrap();
        assert_eq!(model_selector.right(), model_field.right());
        assert_eq!(model_selector.size.width, model_field.size.width);
        assert!(model_selector.left() >= model_field.left());

        let thinking_field = cx.debug_bounds("setting-item-field-3-0-3").unwrap();
        let thinking_selector = cx.debug_bounds("setting-dropdown-3-0-3").unwrap();
        assert_eq!(thinking_selector.right(), thinking_field.right());
        assert!(thinking_selector.size.width < thinking_field.size.width);
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
        let model_divider_selector: &'static str =
            Box::leak(format!("provider-divider-{host}-models").into_boxed_str());
        let chips_selector: &'static str =
            Box::leak(format!("provider-model-chips-{host}").into_boxed_str());
        let chip_selector: &'static str =
            Box::leak(format!("provider-model-chip-{host}-local-model").into_boxed_str());
        let trash_selector: &'static str =
            Box::leak(format!("provider-model-trash-{host}").into_boxed_str());
        let actions_selector: &'static str =
            Box::leak(format!("provider-model-actions-{host}").into_boxed_str());
        let api_host = cx
            .debug_bounds(api_host_selector)
            .expect("API Host input host");
        let api_key = cx
            .debug_bounds(api_key_selector)
            .expect("API Key input host");
        let api_host_row = cx.debug_bounds("setting-item-4-1-2").expect("API Host row");
        let api_host_label = cx
            .debug_bounds("setting-item-label-4-1-2")
            .expect("API Host label");
        let api_host_field = cx
            .debug_bounds("setting-item-field-4-1-2")
            .expect("API Host field");
        let api_key_row = cx.debug_bounds("setting-item-4-1-3").expect("API Key row");
        let api_key_label = cx
            .debug_bounds("setting-item-label-4-1-3")
            .expect("API Key label");
        let api_key_field = cx
            .debug_bounds("setting-item-field-4-1-3")
            .expect("API Key field");
        for (row, label, field, input) in [
            (api_host_row, api_host_label, api_host_field, api_host),
            (api_key_row, api_key_label, api_key_field, api_key),
        ] {
            assert_eq!(field.right(), row.right());
            assert_eq!(field.size.width, px(480.0));
            assert_eq!(input, field);
            assert!(field.left() > label.right());
        }
        assert_eq!(api_host_field.left(), api_key_field.left());
        assert_eq!(api_host_field.right(), api_key_field.right());

        let compatibility_divider = cx
            .debug_bounds(compatibility_divider_selector)
            .expect("Compatibility divider");
        let credential_divider = cx
            .debug_bounds(credential_divider_selector)
            .expect("Credential divider");
        let model_divider = cx
            .debug_bounds(model_divider_selector)
            .expect("Model divider");
        for divider in [compatibility_divider, credential_divider, model_divider] {
            assert_eq!(divider.size.height, px(1.0));
            assert!(divider.size.width > px(400.0));
        }

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
        let actions = cx.debug_bounds(actions_selector).expect("model actions");
        let active_model_divider = cx
            .debug_bounds(model_divider_selector)
            .expect("active Model divider");
        assert_eq!(trash.size.height, px(48.0));
        assert!(trash.size.width > px(400.0));
        assert!(trash.top() >= chips.bottom());
        assert!(active_model_divider.top() >= trash.bottom());
        assert!(actions.top() >= active_model_divider.bottom());

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
