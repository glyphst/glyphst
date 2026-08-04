use super::super::SettingsWindow;
use glyphst_settings::{
    DocumentTranslationMode, ModelRef, PdfContentReviewMode, Settings, SettingsStore,
    TranslationBoxCorner, TranslationProvider,
};
use glyphst_translation_ui::target_language_options;
use gpui::{App, Context, SharedString};
use gpui_component::{
    Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage},
};

impl SettingsWindow {
    fn translation_provider_group(
        &self,
        provider: TranslationProvider,
        cx: &mut Context<Self>,
    ) -> SettingGroup {
        let view = cx.entity();
        let api_key_field = Self::bound_text_field(
            format!("translation-api-key-{}", provider.as_str()),
            crate::fl!("settings-api-key-placeholder"),
            true,
            move |cx| {
                cx.global::<SettingsStore>()
                    .settings
                    .translation
                    .provider_config(provider)
                    .api_key
                    .into()
            },
            {
                let view = view.clone();
                move |value, cx| {
                    Self::update_text_settings(&view, cx, move |settings| {
                        settings.translation.provider_config_mut(provider).api_key =
                            value.to_string();
                    });
                }
            },
        )
        .on_reset(
            move |cx| {
                !cx.global::<SettingsStore>()
                    .settings
                    .translation
                    .provider_config(provider)
                    .api_key
                    .is_empty()
            },
            {
                let view = view.clone();
                move |_, cx| {
                    Self::update_settings(&view, cx, move |settings| {
                        settings
                            .translation
                            .provider_config_mut(provider)
                            .api_key
                            .clear();
                    });
                }
            },
        );
        let api_key = SettingItem::new(crate::fl!("settings-api-key"), api_key_field);

        let default_base_url = provider
            .default_base_url()
            .expect("non-AI translation providers have an API endpoint");
        let base_url_field = Self::bound_text_field(
            format!("translation-base-url-{}", provider.as_str()),
            default_base_url,
            false,
            move |cx| {
                cx.global::<SettingsStore>()
                    .settings
                    .translation
                    .provider_config(provider)
                    .base_url
                    .unwrap_or_else(|| default_base_url.to_string())
                    .into()
            },
            {
                let view = view.clone();
                move |value, cx| {
                    Self::update_text_settings(&view, cx, move |settings| {
                        settings.translation.provider_config_mut(provider).base_url =
                            Some(value.to_string());
                    });
                }
            },
        )
        .on_reset(
            move |cx| {
                cx.global::<SettingsStore>()
                    .settings
                    .translation
                    .provider_config(provider)
                    .base_url
                    .is_some()
            },
            {
                let view = view.clone();
                move |_, cx| {
                    Self::update_settings(&view, cx, move |settings| {
                        settings.translation.provider_config_mut(provider).base_url = None;
                    });
                }
            },
        );
        let base_url = SettingItem::new("API endpoint", base_url_field);

        let mut items = vec![api_key, base_url];
        if provider == TranslationProvider::Microsoft {
            let region_field = Self::bound_text_field(
                "translation-microsoft-region",
                "for example: eastus",
                false,
                |cx| {
                    cx.global::<SettingsStore>()
                        .settings
                        .translation
                        .provider_config(TranslationProvider::Microsoft)
                        .region
                        .unwrap_or_default()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_text_settings(&view, cx, move |settings| {
                            settings
                                .translation
                                .provider_config_mut(TranslationProvider::Microsoft)
                                .region = Some(value.to_string());
                        });
                    }
                },
            )
            .on_reset(
                |cx| {
                    cx.global::<SettingsStore>()
                        .settings
                        .translation
                        .provider_config(TranslationProvider::Microsoft)
                        .region
                        .is_some()
                },
                {
                    let view = view.clone();
                    move |_, cx| {
                        Self::update_settings(&view, cx, |settings| {
                            settings
                                .translation
                                .provider_config_mut(TranslationProvider::Microsoft)
                                .region = None;
                        });
                    }
                },
            );
            items.push(SettingItem::new("Azure region", region_field));
        }

        SettingGroup::new().title(provider.label()).items(items)
    }

    pub(super) fn translation_settings_page(&self, cx: &mut Context<Self>) -> SettingPage {
        let view = cx.entity();
        let defaults = Settings::default();

        let target_options = target_language_options(
            &cx.global::<SettingsStore>()
                .settings
                .translation
                .target_language,
        )
        .into_iter()
        .map(|item| Self::setting_option(item.value, item.label))
        .collect();
        let target_language = SettingItem::new(
            "Target language",
            SettingField::scrollable_dropdown(
                target_options,
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .translation
                        .target_language
                        .clone()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.translation.target_language = value.to_string();
                        });
                    }
                },
            )
            .default_value(defaults.translation.target_language.clone()),
        );

        let document_mode = SettingItem::new(
            "Document translation mode",
            SettingField::dropdown(
                DocumentTranslationMode::ALL
                    .into_iter()
                    .map(|mode| Self::setting_option(mode.as_str(), mode.label()))
                    .collect(),
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .translation
                        .document_mode
                        .as_str()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let mode =
                            DocumentTranslationMode::parse(value.as_ref()).unwrap_or_default();
                        Self::update_settings(&view, cx, move |settings| {
                            settings.translation.document_mode = mode;
                        });
                    }
                },
            )
            .default_value(defaults.translation.document_mode.as_str().to_string()),
        )
        .description(
            "Auto uses side-by-side panes in wide views and top/bottom panes in tall views.",
        );

        let active_provider = SettingItem::new(
            "Active provider",
            SettingField::dropdown(
                TranslationProvider::ALL
                    .into_iter()
                    .map(|provider| Self::setting_option(provider.as_str(), provider.label()))
                    .collect(),
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .translation
                        .active_provider
                        .as_str()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let provider =
                            TranslationProvider::parse(value.as_ref()).unwrap_or_default();
                        Self::update_settings(&view, cx, move |settings| {
                            settings.translation.active_provider = provider;
                        });
                    }
                },
            )
            .default_value(defaults.translation.active_provider.as_str().to_string()),
        );

        let model_options = Self::model_options(&cx.global::<SettingsStore>().settings);
        let default_translation_model = defaults
            .ai
            .translation_model
            .as_ref()
            .map(Self::model_ref_key)
            .unwrap_or_default();
        let ai_model = SettingItem::new(
            crate::fl!("settings-translation-model"),
            SettingField::scrollable_dropdown(
                model_options,
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .ai
                        .translation_model
                        .as_ref()
                        .map(Self::model_ref_key)
                        .unwrap_or_default()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let model: Option<ModelRef> = Self::parse_model_ref(value.as_ref());
                        Self::update_settings(&view, cx, move |settings| {
                            settings.ai.translation_model = model;
                        });
                    }
                },
            )
            .default_value(default_translation_model),
        );

        let content_review_mode = SettingItem::new(
            "AI content review",
            SettingField::dropdown(
                PdfContentReviewMode::ALL
                    .into_iter()
                    .map(|mode| Self::setting_option(mode.as_str(), mode.label()))
                    .collect(),
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .translation
                        .content_review
                        .mode
                        .as_str()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let mode = PdfContentReviewMode::parse(value.as_ref()).unwrap_or_default();
                        Self::update_settings(&view, cx, move |settings| {
                            settings.translation.content_review.mode = mode;
                            if mode == PdfContentReviewMode::Ambiguous
                                && settings.translation.content_review.model.is_none()
                            {
                                settings.translation.content_review.model =
                                    settings.ai.translation_model.clone();
                            }
                        });
                    }
                },
            )
            .default_value(defaults.translation.content_review.mode.as_str().to_string()),
        )
        .description(
            "Optionally ask a separate AI model about only the blocks whose translate/preserve policy is ambiguous. Deterministic decisions remain locked.",
        );

        let default_review_model = defaults
            .translation
            .content_review
            .model
            .as_ref()
            .map(Self::model_ref_key)
            .unwrap_or_default();
        let content_review_model = SettingItem::new(
            "Review model",
            SettingField::scrollable_dropdown(
                Self::model_options(&cx.global::<SettingsStore>().settings),
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .translation
                        .content_review
                        .model
                        .as_ref()
                        .map(Self::model_ref_key)
                        .unwrap_or_default()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let model = Self::parse_model_ref(value.as_ref());
                        Self::update_settings(&view, cx, move |settings| {
                            settings.translation.content_review.model = model;
                        });
                    }
                },
            )
            .default_value(default_review_model),
        )
        .disabled(
            cx.global::<SettingsStore>()
                .settings
                .translation
                .content_review
                .mode
                == PdfContentReviewMode::Off,
        );

        let pinned_width = SettingItem::new(
            "Floating panel width",
            SettingField::number_input(
                NumberFieldOptions {
                    min: 240.0,
                    max: 640.0,
                    step: 8.0,
                },
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .translation
                        .pinned
                        .width as f64
                },
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.translation.pinned.width = value as f32;
                        });
                    }
                },
            )
            .default_value(defaults.translation.pinned.width as f64),
        );

        let pinned_corner = SettingItem::new(
            "Panel corner",
            SettingField::dropdown(
                vec![
                    Self::setting_option("top-left", "Top left"),
                    Self::setting_option("top-right", "Top right"),
                    Self::setting_option("bottom-left", "Bottom left"),
                    Self::setting_option("bottom-right", "Bottom right"),
                ],
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .translation
                        .pinned
                        .corner
                        .as_str()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let corner =
                            TranslationBoxCorner::parse(value.as_ref()).unwrap_or_default();
                        Self::update_settings(&view, cx, move |settings| {
                            settings.translation.pinned.corner = corner;
                        });
                    }
                },
            )
            .default_value(defaults.translation.pinned.corner.as_str().to_string()),
        );

        let clear_view = view.clone();
        let clear_data = SettingItem::new(
            "Clear translation data",
            SettingField::render(move |options, _, cx| {
                let clear_view = clear_view.clone();
                let state = clear_view.read(cx);
                let running = state.translation_data_clear_running;
                let pending = state.translation_data_clear_pending;
                Button::new("clear-translation-data")
                    .label(if running {
                        "Clearing…"
                    } else {
                        "Clear"
                    })
                    .danger()
                    .outline()
                    .with_size(options.size)
                    .loading(running)
                    .disabled(pending)
                    .on_click(move |_, window, cx| {
                        clear_view.update(cx, |this, cx| {
                            this.clear_translation_data(window, cx);
                        });
                    })
            }),
        )
        .description(
            "Delete cached provider responses, generated translated PDFs, OCR results, and resumable checkpoints. Source PDFs and settings are preserved.",
        );

        SettingPage::new(crate::fl!("settings-tab-translation")).groups([
            SettingGroup::new()
                .title(crate::fl!("settings-section-general"))
                .items([target_language, document_mode, active_provider]),
            SettingGroup::new()
                .title("Pinned translation")
                .items([pinned_width, pinned_corner]),
            SettingGroup::new()
                .title(TranslationProvider::Ai.label())
                .item(ai_model),
            SettingGroup::new()
                .title("PDF content policy")
                .items([content_review_mode, content_review_model]),
            self.translation_provider_group(TranslationProvider::DeepL, cx),
            self.translation_provider_group(TranslationProvider::Google, cx),
            self.translation_provider_group(TranslationProvider::Microsoft, cx),
            SettingGroup::new()
                .title("Translation data")
                .item(clear_data),
        ])
    }
}
