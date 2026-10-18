//! The provider pages of the settings window: translation, OCR and AI,
//! with their keys, connection tests, the AI providers' order and the
//! ChatGPT Subscription sign-in. They edit a copy of the user's settings
//! and hand the changes to `UserSettings`, which saves them.

use std::{
    collections::{BTreeSet, HashMap},
    sync::mpsc::{self, Receiver},
    thread,
};

use egui::RichText;
use glyphst_ai::{
    ai_client::{AiModelInfo, list_ai_models, test_ai_connection},
    chatgpt_subscription::ChatGptSubscription,
};
use glyphst_pdf_translation_service::{OcrProbeError, test_ocr_connection};
use glyphst_settings::{
    AiApiStyle, AiModel, AiProvider, ModelRef, OcrMode, OcrProvider, OcrSettings, ProviderSecretId,
    ResolvedAiConfig, Settings, TranslationProvider, TranslationTargetLanguage,
};

use crate::{
    icons,
    theme::{self, Tokens},
    user_settings::{SecretStatus, UserSettings},
};
use glyphst_ui::settings::Group;

/// One AI provider's row: which it is, where it is in the order, and the
/// settings it edits.
struct ProviderRow<'a> {
    provider: AiProvider,
    index: usize,
    count: usize,
    current: &'a Settings,
    draft: &'a mut Settings,
    user: &'a UserSettings,
    chatgpt: &'a ChatGptSubscription,
}

/// What the providers' rows asked for, done after them.
struct ProviderEdits<'a> {
    remove: &'a mut Option<AiProvider>,
    reorder: &'a mut Option<(usize, usize)>,
    fetch: &'a mut Option<AiProvider>,
    test: &'a mut Option<AiProvider>,
}

/// A provider's models being listed.
type Fetch = (AiProvider, Receiver<Result<Vec<AiModelInfo>, String>>);

/// A connection test: what it tested, and its result once done. A result
/// for settings that changed meanwhile is not shown as current.
struct Test<C, E> {
    config: C,
    result: Option<Result<(), E>>,
    receiver: Option<Receiver<Result<(), E>>>,
}

impl<C: PartialEq, E> Test<C, E> {
    fn start(
        config: C,
        ctx: &egui::Context,
        run: impl FnOnce() -> Result<(), E> + Send + 'static,
    ) -> Self
    where
        E: Send + 'static,
    {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        let _ = thread::Builder::new()
            .name("connection-test".into())
            .spawn(move || {
                let _ = sender.send(run());
                ctx.request_repaint();
            });
        Self {
            config,
            result: None,
            receiver: Some(receiver),
        }
    }

    fn poll(&mut self) {
        if let Some(result) = self.receiver.as_ref().and_then(|r| r.try_recv().ok()) {
            self.result = Some(result);
            self.receiver = None;
        }
    }

    fn running(&self) -> bool {
        self.receiver.is_some()
    }
}

/// A test's status line: what it found, or that the settings changed.
fn test_status<C: PartialEq, E: std::fmt::Display>(
    ui: &mut egui::Ui,
    test: Option<&Test<C, E>>,
    current: &C,
) {
    let tokens = Tokens::get(ui.ctx());
    let Some(test) = test else {
        return;
    };
    if test.running() {
        ui.spinner();
        ui.label(RichText::new("Testing…").color(tokens.muted));
        return;
    }
    let (text, color) = match &test.result {
        _ if test.config != *current => (
            "Settings changed since the test; test again.".to_string(),
            tokens.muted,
        ),
        Some(Ok(())) => ("Connected".to_string(), tokens.muted),
        Some(Err(error)) => (format!("Failed: {error}"), tokens.err),
        None => return,
    };
    ui.label(RichText::new(text).small().color(color));
}

/// State of the provider pages between frames.
#[derive(Default)]
pub struct ProviderPages {
    /// Keys shown in clear.
    revealed: BTreeSet<ProviderSecretId>,
    fetching: Option<Fetch>,
    fetched: HashMap<AiProvider, Result<Vec<AiModelInfo>, String>>,
    new_model: HashMap<AiProvider, String>,
    new_provider: (String, String),
    ai_tests: HashMap<AiProvider, Test<ResolvedAiConfig, String>>,
    ocr_test: Option<Test<OcrSettings, OcrProbeError>>,
    /// Sign-in started here: ChatGPT Subscription turns on when it is done.
    signing_in: bool,
}

impl ProviderPages {
    /// Takes a finished model list and finished connection tests.
    pub fn poll(&mut self) {
        if let Some((provider, receiver)) = &self.fetching
            && let Ok(result) = receiver.try_recv()
        {
            self.fetched.insert(*provider, result);
            self.fetching = None;
        }
        for test in self.ai_tests.values_mut() {
            test.poll();
        }
        if let Some(test) = &mut self.ocr_test {
            test.poll();
        }
    }

    /// Tests `provider`'s connection on a thread.
    pub fn test_ai(&mut self, settings: &Settings, provider: AiProvider, ctx: &egui::Context) {
        let config = settings.ai.resolved_provider(provider);
        let run = config.clone();
        let test = Test::start(config, ctx, move || {
            test_ai_connection(run).map_err(|error| format!("{error:#}"))
        });
        self.ai_tests.insert(provider, test);
    }

    /// Lists `provider`'s models on a thread.
    pub fn fetch_models(&mut self, settings: &Settings, provider: AiProvider, ctx: &egui::Context) {
        let config = settings.ai.resolved_provider(provider);
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        let spawned = thread::Builder::new()
            .name("list-models".into())
            .spawn(move || {
                let result = list_ai_models(config).map_err(|error| format!("{error:#}"));
                let _ = sender.send(result);
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => self.fetching = Some((provider, receiver)),
            Err(error) => {
                self.fetched.insert(provider, Err(error.to_string()));
            }
        }
    }

    pub fn is_fetching(&self) -> bool {
        self.fetching.is_some()
    }

    pub fn translation(&mut self, ui: &mut egui::Ui, user: &mut UserSettings) {
        let Some(current) = user.draft().cloned() else {
            loading(ui);
            return;
        };
        let mut draft = Settings::clone(&current);
        let translation = &mut draft.translation;
        glyphst_ui::settings::group(ui, "Service", |group| {
            group.row("Service", None, |ui| {
                egui::ComboBox::from_id_salt("translation-provider")
                    .selected_text(translation.active_provider.label())
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for provider in TranslationProvider::ALL {
                            ui.selectable_value(
                                &mut translation.active_provider,
                                provider,
                                provider.label(),
                            );
                        }
                    });
            });
            let provider = translation.active_provider;
            if provider == TranslationProvider::Ai {
                group.note("Translates with the AI translation model (AI page).");
                return;
            }
            let id = ProviderSecretId::TranslationApiKey(provider);
            let config = translation.provider_config_mut(provider);
            self.secret(group, "API key", &mut config.api_key, id, user.status(id));
            optional_field(
                group,
                "Host",
                &mut config.base_url,
                provider.default_base_url().unwrap_or_default(),
            );
            if provider == TranslationProvider::Microsoft {
                optional_field(group, "Region", &mut config.region, "global");
            }
        });
        glyphst_ui::settings::group(ui, "Language", |group| {
            let chosen = match &translation.target_language {
                TranslationTargetLanguage::UiLanguage => "The interface language".to_string(),
                TranslationTargetLanguage::Explicit(code) => language_label(code),
            };
            group.row(
                "Translate into",
                Some("The toolbar's language menu overrides this in this app."),
                |ui| {
                    egui::ComboBox::from_id_salt("translation-target")
                        .selected_text(chosen)
                        .width(220.0)
                        .height(360.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut translation.target_language,
                                TranslationTargetLanguage::UiLanguage,
                                "The interface language",
                            );
                            for language in glyphst_translation::TARGET_LANGUAGES {
                                ui.selectable_value(
                                    &mut translation.target_language,
                                    TranslationTargetLanguage::Explicit(language.code.into()),
                                    language.label,
                                );
                            }
                        });
                },
            );
        });
        apply(user, &current, draft);
    }

    pub fn ocr(&mut self, ui: &mut egui::Ui, user: &mut UserSettings) {
        let Some(current) = user.draft().cloned() else {
            loading(ui);
            return;
        };
        let mut draft = Settings::clone(&current);
        let ocr = &mut draft.ocr;
        glyphst_ui::settings::group(ui, "Recognition", |group| {
            group.row(
                "Recognize text",
                Some("In scanned pages, for selection, search and translation."),
                |ui| {
                    glyphst_ui::segmented::labels(
                        ui,
                        &mut ocr.mode,
                        &[
                            (OcrMode::Always, "Always"),
                            (OcrMode::Auto, "When a page has no text"),
                            (OcrMode::Off, "Never"),
                        ],
                    );
                },
            );
        });
        glyphst_ui::settings::group(ui, "Service", |group| {
            group.row("Service", None, |ui| {
                glyphst_ui::segmented::labels(
                    ui,
                    &mut ocr.provider,
                    &[
                        (OcrProvider::Unlimited, "Baidu Unlimited OCR"),
                        (OcrProvider::Paddle, "PaddleOCR"),
                    ],
                );
            });
            match ocr.provider {
                OcrProvider::Paddle => {
                    let id = ProviderSecretId::PaddleOcrToken;
                    self.secret(group, "Token", &mut ocr.paddle.token, id, user.status(id));
                    text_field(group, "Endpoint", &mut ocr.paddle.endpoint, "");
                    text_field(group, "Model", &mut ocr.paddle.model, "");
                }
                OcrProvider::Unlimited => {
                    let id = ProviderSecretId::UnlimitedOcrApiKey;
                    self.secret(
                        group,
                        "API key",
                        &mut ocr.unlimited.api_key,
                        id,
                        user.status(id),
                    );
                    let id = ProviderSecretId::UnlimitedOcrSecretKey;
                    self.secret(
                        group,
                        "Secret key",
                        &mut ocr.unlimited.secret_key,
                        id,
                        user.status(id),
                    );
                    text_field(group, "Endpoint", &mut ocr.unlimited.endpoint, "");
                }
            }
            group.row("Connection", None, |ui| {
                let running = self.ocr_test.as_ref().is_some_and(Test::running);
                if ui
                    .add_enabled(!running, egui::Button::new("Test connection"))
                    .clicked()
                {
                    let config = current.ocr.clone();
                    let run = config.clone();
                    self.ocr_test = Some(Test::start(config, ui.ctx(), move || {
                        test_ocr_connection(run)
                    }));
                }
                test_status(ui, self.ocr_test.as_ref(), &current.ocr);
            });
        });
        apply(user, &current, draft);
    }

    pub fn ai(
        &mut self,
        ui: &mut egui::Ui,
        user: &mut UserSettings,
        chatgpt: &ChatGptSubscription,
    ) {
        let Some(current) = user.draft().cloned() else {
            loading(ui);
            return;
        };
        let mut draft = Settings::clone(&current);
        let mut fetch = None;
        let mut test = None;
        // A sign-in started here turns the provider on once it is done.
        let auth = chatgpt.snapshot();
        if self.signing_in && !auth.signing_in {
            self.signing_in = false;
            if auth.is_authenticated() {
                draft
                    .ai
                    .set_provider_enabled(AiProvider::ChatGptSubscription, true);
            }
        }
        let choices = model_choices(&draft);
        glyphst_ui::settings::group(ui, "Models", |group| {
            group.row("Assistant", None, |ui| {
                model_picker(ui, "main-model", &mut draft.ai.main_model, &choices, None);
            });
            group.row("Translation", Some("For AI translation."), |ui| {
                model_picker(
                    ui,
                    "translation-model",
                    &mut draft.ai.translation_model,
                    &choices,
                    Some("Same as the assistant"),
                );
            });
            if choices.is_empty() {
                group.note("Turn a provider on and give it a model to choose one.");
            }
        });
        let mut remove = None;
        let mut reorder = None;
        let order = draft.ai.provider_order.clone();
        glyphst_ui::settings::group(ui, "Providers", |group| {
            group.note("The first provider turned on answers when a model is not chosen.");
            for (index, provider) in order.iter().copied().enumerate() {
                group.full(|ui| {
                    self.provider(
                        ui,
                        ProviderRow {
                            provider,
                            index,
                            count: order.len(),
                            current: &current,
                            draft: &mut draft,
                            user: &*user,
                            chatgpt,
                        },
                        &mut ProviderEdits {
                            remove: &mut remove,
                            reorder: &mut reorder,
                            fetch: &mut fetch,
                            test: &mut test,
                        },
                    );
                });
            }
        });
        if let Some(provider) = remove {
            draft.ai.remove_custom_provider(provider);
        }
        if let Some((from, to)) = reorder {
            draft.ai.provider_order.swap(from, to);
        }
        glyphst_ui::settings::group(ui, "Add a provider", |group| {
            group.row(
                "OpenAI-compatible",
                Some("Any service with an OpenAI-style API: a name and its host."),
                |ui| {
                    let (name, host) = &mut self.new_provider;
                    let ready = !name.trim().is_empty() && !host.trim().is_empty();
                    if ui.add_enabled(ready, egui::Button::new("Add")).clicked() {
                        let provider = draft.ai.add_custom_provider(
                            name.trim(),
                            AiApiStyle::OpenAi,
                            host.trim(),
                            "",
                        );
                        draft.ai.set_provider_enabled(provider, true);
                        name.clear();
                        host.clear();
                    }
                    glyphst_ui::text::fitted(ui, |ui| {
                        ui.add(
                            egui::TextEdit::singleline(host)
                                .margin(glyphst_ui::text::FIELD_MARGIN)
                                .hint_text("https://host/v1")
                                .desired_width(200.0),
                        )
                    });
                    glyphst_ui::text::fitted(ui, |ui| {
                        ui.add(
                            egui::TextEdit::singleline(name)
                                .margin(glyphst_ui::text::FIELD_MARGIN)
                                .hint_text("Name")
                                .desired_width(120.0),
                        )
                    });
                },
            );
        });
        apply(user, &current, draft);
        if let (Some(provider), Some(settings)) = (fetch, user.get().cloned()) {
            self.fetch_models(&settings, provider, ui.ctx());
        }
        if let (Some(provider), Some(settings)) = (test, user.get().cloned()) {
            self.test_ai(&settings, provider, ui.ctx());
        }
    }

    /// One AI provider: a header that opens to its switch, order, test,
    /// key, host and models.
    fn provider(&mut self, ui: &mut egui::Ui, row: ProviderRow<'_>, edits: &mut ProviderEdits<'_>) {
        let tokens = Tokens::get(ui.ctx());
        let ProviderRow {
            provider,
            index,
            count,
            current,
            draft,
            user,
            chatgpt,
        } = row;
        let config = draft.ai.provider_config(provider);
        let title = if config.enabled {
            config.name.clone()
        } else {
            format!("{} (off)", config.name)
        };
        let listing = self.fetched.contains_key(&provider)
            || self.fetching.as_ref().is_some_and(|(p, _)| *p == provider);
        egui::CollapsingHeader::new(RichText::new(title).font(theme::medium(13.0)))
            .id_salt(("ai-provider", provider.as_str()))
            .default_open(listing)
            .show(ui, |ui| {
                let mut enabled = config.enabled;
                ui.horizontal(|ui| {
                    if ui.add(glyphst_ui::toggle::Switch(&mut enabled)).changed() {
                        draft.ai.set_provider_enabled(provider, enabled);
                    }
                    ui.label(if enabled { "On" } else { "Off" });
                    ui.add_space(8.0);
                    ui.add_enabled_ui(index > 0, |ui| {
                        if icons::button(ui, "arrow-up", 24.0, false, "Move up").clicked() {
                            *edits.reorder = Some((index, index - 1));
                        }
                    });
                    ui.add_enabled_ui(index + 1 < count, |ui| {
                        if icons::button(ui, "arrow-down", 24.0, false, "Move down").clicked() {
                            *edits.reorder = Some((index, index + 1));
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let running = self.ai_tests.get(&provider).is_some_and(Test::running);
                        if ui
                            .add_enabled(!running, egui::Button::new("Test connection"))
                            .clicked()
                        {
                            *edits.test = Some(provider);
                        }
                        test_status(
                            ui,
                            self.ai_tests.get(&provider),
                            &current.ai.resolved_provider(provider),
                        );
                    });
                });
                if provider == AiProvider::ChatGptSubscription {
                    self.chatgpt_account(ui, chatgpt);
                    return;
                }
                let id = ProviderSecretId::AiApiKey(provider);
                let status = user.status(id);
                let config = draft.ai.provider_config_mut(provider);
                glyphst_ui::settings::group(ui, "", |group| {
                    self.secret(group, "API key", &mut config.api_key, id, status);
                    if provider.is_custom() {
                        text_field(group, "Name", &mut config.name, "");
                        optional_field(group, "Host", &mut config.base_url, "https://…/v1");
                    }
                    group.full(|ui| self.models(ui, provider, config, edits.fetch, &tokens));
                });
                if provider.is_custom() && ui.button("Remove this provider").clicked() {
                    *edits.remove = Some(provider);
                }
            });
    }

    /// A provider's models: the listed ones, adding one by id, and the
    /// ones fetched from the provider.
    fn models(
        &mut self,
        ui: &mut egui::Ui,
        provider: AiProvider,
        config: &mut glyphst_settings::AiProviderConfig,
        fetch: &mut Option<AiProvider>,
        tokens: &Tokens,
    ) {
        ui.label(RichText::new("Models").font(theme::medium(13.0)));
        let mut drop = None;
        for model in &config.models {
            ui.horizontal(|ui| {
                ui.label(&model.name);
                if model.name != model.id {
                    ui.label(RichText::new(&model.id).small().color(tokens.muted));
                }
                if ui
                    .push_id(&model.id, |ui| {
                        icons::button(ui, "x", 22.0, false, "Remove")
                    })
                    .inner
                    .clicked()
                {
                    drop = Some(model.id.clone());
                }
            });
        }
        if let Some(id) = drop {
            config.remove_model(&id);
        }
        ui.horizontal(|ui| {
            let new = self.new_model.entry(provider).or_default();
            glyphst_ui::text::fitted(ui, |ui| {
                ui.add(
                    egui::TextEdit::singleline(new)
                        .margin(glyphst_ui::text::FIELD_MARGIN)
                        .hint_text("model id")
                        .desired_width(200.0),
                )
            });
            if ui
                .add_enabled(!new.trim().is_empty(), egui::Button::new("Add"))
                .clicked()
            {
                let id = new.trim().to_string();
                config.ensure_model(AiModel::new(id.clone(), id, config.name.clone()));
                new.clear();
            }
            let busy = self.is_fetching();
            if ui
                .add_enabled(!busy, egui::Button::new("Fetch models"))
                .on_hover_text("List the models the provider offers")
                .clicked()
            {
                *fetch = Some(provider);
            }
            if busy && self.fetching.as_ref().is_some_and(|(p, _)| *p == provider) {
                ui.spinner();
            }
        });
        match self.fetched.get(&provider) {
            Some(Ok(found)) => {
                let missing: Vec<&AiModelInfo> = found
                    .iter()
                    .filter(|info| config.model(&info.id).is_none())
                    .collect();
                if missing.is_empty() {
                    ui.label(
                        RichText::new("All the provider's models are listed.").color(tokens.muted),
                    );
                }
                egui::ScrollArea::vertical()
                    .id_salt(("fetched", provider.as_str()))
                    .max_height(160.0)
                    .show(ui, |ui| {
                        for info in missing {
                            ui.horizontal(|ui| {
                                if ui.small_button("+").clicked() {
                                    config.ensure_model(AiModel::new(
                                        info.id.clone(),
                                        info.label.clone(),
                                        config.name.clone(),
                                    ));
                                }
                                ui.label(&info.label);
                            });
                        }
                    });
            }
            Some(Err(error)) => {
                ui.colored_label(tokens.err, error);
            }
            None => {}
        }
    }

    /// The ChatGPT account: who is signed in, and signing in or out.
    fn chatgpt_account(&mut self, ui: &mut egui::Ui, chatgpt: &ChatGptSubscription) {
        let tokens = Tokens::get(ui.ctx());
        let auth = chatgpt.snapshot();
        ui.horizontal(|ui| {
            if auth.loading {
                ui.spinner();
                ui.label(RichText::new("Reading the saved sign-in…").color(tokens.muted));
            } else if auth.signing_in {
                ui.spinner();
                ui.label(RichText::new("Finish signing in in your browser…").color(tokens.muted));
            } else if let Some(email) = &auth.email {
                ui.label(format!("Signed in as {email}"));
                if ui
                    .add(glyphst_ui::button::Button::plain("sign out").icon("log-out"))
                    .clicked()
                {
                    chatgpt.sign_out();
                }
            } else if ui
                .add(glyphst_ui::button::Button::primary("sign in with chatgpt").icon("log-in"))
                .clicked()
            {
                match chatgpt.begin_sign_in() {
                    Ok(url) => {
                        self.signing_in = true;
                        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                    }
                    Err(_) => self.signing_in = false,
                }
            }
        });
        if let Some(error) = &auth.error {
            ui.label(RichText::new(error).small().color(tokens.err));
        }
        ui.label(
            RichText::new("Uses your ChatGPT plan; the sign-in is kept in the system keychain.")
                .small()
                .color(tokens.muted),
        );
    }

    /// A key field, hidden unless revealed, with where it is kept.
    fn secret(
        &mut self,
        group: &mut Group<'_>,
        label: &str,
        value: &mut String,
        id: ProviderSecretId,
        status: SecretStatus,
    ) {
        let tokens = Tokens::get(group.ctx());
        let color = match status {
            SecretStatus::NotSaved(_) => tokens.err,
            _ => tokens.muted,
        };
        let description = RichText::new(status.label())
            .font(theme::regular(11.5))
            .color(color);
        group.row_with(label, Some(description), |ui| {
            let shown = self.revealed.contains(&id);
            let (icon, tip) = if shown {
                ("eye-off", "Hide")
            } else {
                ("eye", "Show")
            };
            if ui
                .push_id(("reveal", label), |ui| {
                    icons::button(ui, icon, 24.0, false, tip)
                })
                .inner
                .clicked()
                && !self.revealed.remove(&id)
            {
                self.revealed.insert(id);
            }
            glyphst_ui::text::fitted(ui, |ui| {
                ui.add(
                    egui::TextEdit::singleline(value)
                        .margin(glyphst_ui::text::FIELD_MARGIN)
                        .password(!shown)
                        .hint_text("not set")
                        .desired_width(FIELD_WIDTH - 32.0),
                )
            });
        });
    }
}

/// Hands an edited copy to the user's settings when it changed.
fn apply(user: &mut UserSettings, current: &Settings, draft: Settings) {
    if draft != *current {
        user.edit(move |settings| *settings = draft);
    }
}

fn loading(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label("Reading your settings…");
    });
}

/// The width of a text field in a settings row.
const FIELD_WIDTH: f32 = 280.0;

fn text_field(group: &mut Group<'_>, label: &str, value: &mut String, hint: &str) {
    group.row(label, None, |ui| {
        glyphst_ui::text::fitted(ui, |ui| {
            ui.add(
                egui::TextEdit::singleline(value)
                    .margin(glyphst_ui::text::FIELD_MARGIN)
                    .hint_text(hint)
                    .desired_width(FIELD_WIDTH),
            )
        });
    });
}

/// A field for an optional value; empty means the default (`hint`).
fn optional_field(group: &mut Group<'_>, label: &str, value: &mut Option<String>, hint: &str) {
    let mut text = value.clone().unwrap_or_default();
    text_field(group, label, &mut text, hint);
    let trimmed = text.trim();
    let edited = (!trimmed.is_empty()).then(|| text.clone());
    if edited != *value && !(edited.is_none() && value.as_deref().is_some_and(str::is_empty)) {
        *value = edited;
    }
}

fn language_label(code: &str) -> String {
    glyphst_translation::TARGET_LANGUAGES
        .iter()
        .find(|language| language.code == code)
        .map_or_else(|| code.to_string(), |language| language.label.to_string())
}

/// Every model of a provider that is on, with its label.
pub fn model_choices(settings: &Settings) -> Vec<(ModelRef, String)> {
    let mut choices = Vec::new();
    for &provider in settings.ai.ordered_providers() {
        let config = settings.ai.provider_config(provider);
        if !config.enabled {
            continue;
        }
        for model in &config.models {
            choices.push((
                ModelRef::new(provider, model.id.clone()),
                format!("{} · {}", config.name, model.name),
            ));
        }
    }
    choices
}

/// A model menu; `none` names the empty choice when there is one.
pub fn model_picker(
    ui: &mut egui::Ui,
    id: &str,
    value: &mut Option<ModelRef>,
    choices: &[(ModelRef, String)],
    none: Option<&str>,
) -> bool {
    let shown = value
        .as_ref()
        .and_then(|chosen| choices.iter().find(|(model, _)| model == chosen))
        .map(|(_, label)| label.clone())
        .or_else(|| none.map(str::to_string))
        .unwrap_or_else(|| "Choose a model".into());
    let before = value.clone();
    egui::ComboBox::from_id_salt(id)
        .selected_text(shown)
        .width(280.0)
        .height(360.0)
        .show_ui(ui, |ui| {
            if let Some(none) = none {
                ui.selectable_value(value, None, none);
            }
            for (model, label) in choices {
                ui.selectable_value(value, Some(model.clone()), label);
            }
        });
    *value != before
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::fake_providers::{FAKE_MODELS, start_ai_server, use_fake_ai};

    #[test]
    fn fetched_models_can_be_listed_and_chosen_for_the_assistant() {
        let ctx = egui::Context::default();
        let mut settings = Settings::default();
        use_fake_ai(&mut settings, start_ai_server().unwrap());
        let provider = settings.ai.provider_order[0];
        let mut user = UserSettings::in_memory(&ctx, settings, Default::default());

        let mut pages = ProviderPages::default();
        let current = user.get().cloned().unwrap();
        pages.fetch_models(&current, provider, &ctx);
        let started = Instant::now();
        while pages.is_fetching() {
            assert!(started.elapsed() < Duration::from_secs(30), "timed out");
            std::thread::sleep(Duration::from_millis(10));
            pages.poll();
        }
        let listed: Vec<String> = pages.fetched[&provider]
            .as_ref()
            .expect("the models are listed")
            .iter()
            .map(|info| info.id.clone())
            .collect();
        assert_eq!(listed, FAKE_MODELS.map(|(id, _)| id.to_string()));

        // Adding one and choosing it makes it the assistant's model.
        user.edit(|settings| {
            let config = settings.ai.provider_config_mut(provider);
            config.ensure_model(AiModel::new("fake-large", "Fake large model", "Fake"));
        });
        let choices = model_choices(user.get().unwrap());
        let large = ModelRef::new(provider, "fake-large");
        assert!(choices.iter().any(|(model, _)| *model == large));
        user.edit(|settings| settings.ai.main_model = Some(large.clone()));
        let main = user
            .get()
            .map(|settings| settings.ai.resolved_main())
            .unwrap();
        assert_eq!(
            (main.provider, main.model.as_str()),
            (provider, "fake-large")
        );
    }

    fn wait_for(pages: &mut ProviderPages, provider: AiProvider) {
        let started = Instant::now();
        while pages.ai_tests[&provider].running() {
            assert!(started.elapsed() < Duration::from_secs(30), "timed out");
            std::thread::sleep(Duration::from_millis(10));
            pages.poll();
        }
    }

    #[test]
    fn connection_tests_report_the_provider_and_go_stale_when_settings_change() {
        let ctx = egui::Context::default();
        let mut settings = Settings::default();
        use_fake_ai(&mut settings, start_ai_server().unwrap());
        let provider = settings.ai.provider_order[0];

        let mut pages = ProviderPages::default();
        pages.test_ai(&settings, provider, &ctx);
        wait_for(&mut pages, provider);
        let test = &pages.ai_tests[&provider];
        assert!(matches!(test.result, Some(Ok(()))));
        assert!(test.config == settings.ai.resolved_provider(provider));

        // Another host: the test fails, and the old result is stale.
        let mut moved = settings.clone();
        moved.ai.provider_config_mut(provider).base_url = Some("http://127.0.0.1:9".into());
        assert!(pages.ai_tests[&provider].config != moved.ai.resolved_provider(provider));
        pages.test_ai(&moved, provider, &ctx);
        wait_for(&mut pages, provider);
        assert!(matches!(pages.ai_tests[&provider].result, Some(Err(_))));
    }

    #[test]
    fn the_first_provider_on_answers_when_no_model_is_chosen() {
        let ctx = egui::Context::default();
        let mut settings = Settings::default();
        use_fake_ai(&mut settings, start_ai_server().unwrap());
        let fake = settings.ai.provider_order[0];
        let other =
            settings
                .ai
                .add_custom_provider("Other", AiApiStyle::OpenAi, "http://127.0.0.1:9", "");
        settings.ai.set_provider_enabled(other, true);
        let config = settings.ai.provider_config_mut(other);
        config.ensure_model(AiModel::new("other-model", "Other", "Other"));
        settings.ai.main_model = None;
        let mut user = UserSettings::in_memory(&ctx, settings, Default::default());
        let main = |user: &UserSettings| user.get().unwrap().ai.resolved_main().provider;
        assert_eq!(main(&user), fake);

        // Moving the other provider above it, as the up button does.
        let from = user
            .get()
            .unwrap()
            .ai
            .provider_order
            .iter()
            .position(|&p| p == other)
            .unwrap();
        user.edit(|settings| {
            for index in (1..=from).rev() {
                settings.ai.provider_order.swap(index, index - 1);
            }
        });
        assert_eq!(main(&user), other);
    }
}
