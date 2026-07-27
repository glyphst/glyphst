use super::super::SettingsWindow;
use glyphst_settings::{
    AiProvider, AiThinkingLevel, PageImageQuality, PageTextFormat, Settings, SettingsStore,
};
use gpui::{App, Axis, Context, SharedString};
use gpui_component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
};

#[derive(Clone, Copy)]
enum ModelTarget {
    Main,
    Title,
    Translation,
}

impl SettingsWindow {
    fn model_setting_item(
        &self,
        title: impl Into<SharedString>,
        description: impl Into<gpui_component::text::Text>,
        target: ModelTarget,
        cx: &mut Context<Self>,
    ) -> SettingItem {
        let view = cx.entity();
        let options = Self::model_options(&cx.global::<SettingsStore>().settings);
        let defaults = Settings::default();
        let default_value = match target {
            ModelTarget::Main => defaults.ai.main_model.as_ref(),
            ModelTarget::Title => defaults.ai.title_model.as_ref(),
            ModelTarget::Translation => defaults.ai.translation_model.as_ref(),
        }
        .map(Self::model_ref_key)
        .unwrap_or_default();

        SettingItem::new(
            title,
            SettingField::scrollable_dropdown(
                options,
                move |cx: &App| {
                    let settings = &cx.global::<SettingsStore>().settings;
                    let model_ref = match target {
                        ModelTarget::Main => settings.ai.main_model.as_ref(),
                        ModelTarget::Title => settings.ai.title_model.as_ref(),
                        ModelTarget::Translation => settings.ai.translation_model.as_ref(),
                    };
                    model_ref
                        .map(Self::model_ref_key)
                        .unwrap_or_default()
                        .into()
                },
                move |value: SharedString, cx: &mut App| {
                    let model_ref = Self::parse_model_ref(value.as_ref());
                    Self::update_settings(&view, cx, move |settings| match target {
                        ModelTarget::Main => settings.ai.main_model = model_ref,
                        ModelTarget::Title => settings.ai.title_model = model_ref,
                        ModelTarget::Translation => settings.ai.translation_model = model_ref,
                    });
                },
            )
            .default_value(default_value),
        )
        .description(description)
    }

    pub(super) fn ai_settings_page(&self, cx: &mut Context<Self>) -> SettingPage {
        let view = cx.entity();
        let defaults = Settings::default();

        let main_model = self.model_setting_item(
            crate::fl!("settings-main-model"),
            "Used for chat, document questions, and quick ask.",
            ModelTarget::Main,
            cx,
        );
        let title_model = self.model_setting_item(
            crate::fl!("settings-title-model"),
            "Used for lightweight conversation-title generation.",
            ModelTarget::Title,
            cx,
        );
        let translation_model = self.model_setting_item(
            crate::fl!("settings-translation-model"),
            "Used when the AI translation provider is active.",
            ModelTarget::Translation,
            cx,
        );

        let thinking = SettingItem::new(
            "Thinking level",
            SettingField::dropdown(
                [
                    AiThinkingLevel::Off,
                    AiThinkingLevel::Minimal,
                    AiThinkingLevel::Low,
                    AiThinkingLevel::Medium,
                    AiThinkingLevel::High,
                    AiThinkingLevel::XHigh,
                    AiThinkingLevel::Max,
                ]
                .into_iter()
                .map(|level| Self::setting_option(level.as_str(), level.label()))
                .collect(),
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .ai
                        .thinking_level
                        .as_str()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let level = match value.as_ref() {
                            "off" => AiThinkingLevel::Off,
                            "minimal" => AiThinkingLevel::Minimal,
                            "low" => AiThinkingLevel::Low,
                            "high" => AiThinkingLevel::High,
                            "xhigh" => AiThinkingLevel::XHigh,
                            "max" => AiThinkingLevel::Max,
                            _ => AiThinkingLevel::Medium,
                        };
                        Self::update_settings(&view, cx, move |settings| {
                            settings.ai.thinking_level = level;
                        });
                    }
                },
            )
            .default_value(defaults.ai.thinking_level.as_str().to_string()),
        )
        .description("Unsupported levels are automatically clamped for the selected model.");

        let text_format = SettingItem::new(
            crate::fl!("settings-page-text-format"),
            SettingField::dropdown(
                vec![
                    Self::setting_option(
                        PageTextFormat::Block.as_str(),
                        crate::fl!("settings-page-text-format-block"),
                    ),
                    Self::setting_option(
                        PageTextFormat::Semantic.as_str(),
                        crate::fl!("settings-page-text-format-semantic"),
                    ),
                ],
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .ai
                        .page_text_format
                        .as_str()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let format = if value.as_ref() == "semantic" {
                            PageTextFormat::Semantic
                        } else {
                            PageTextFormat::Block
                        };
                        Self::update_settings(&view, cx, move |settings| {
                            settings.ai.page_text_format = format;
                        });
                    }
                },
            )
            .default_value(defaults.ai.page_text_format.as_str().to_string()),
        )
        .description(crate::fl!("settings-page-text-format-hint"));

        let image_quality = SettingItem::new(
            crate::fl!("settings-page-image-quality"),
            SettingField::dropdown(
                vec![
                    Self::setting_option(
                        PageImageQuality::Low.as_str(),
                        crate::fl!("settings-page-image-quality-low"),
                    ),
                    Self::setting_option(
                        PageImageQuality::Medium.as_str(),
                        crate::fl!("settings-page-image-quality-medium"),
                    ),
                    Self::setting_option(
                        PageImageQuality::High.as_str(),
                        crate::fl!("settings-page-image-quality-high"),
                    ),
                ],
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .ai
                        .page_image_quality
                        .as_str()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let quality = match value.as_ref() {
                            "low" => PageImageQuality::Low,
                            "high" => PageImageQuality::High,
                            _ => PageImageQuality::Medium,
                        };
                        Self::update_settings(&view, cx, move |settings| {
                            settings.ai.page_image_quality = quality;
                        });
                    }
                },
            )
            .default_value(defaults.ai.page_image_quality.as_str().to_string()),
        )
        .description(crate::fl!("settings-page-image-quality-hint"));

        let page_image_limit = SettingItem::new(
            crate::fl!("settings-page-image-limit"),
            SettingField::number_input(
                NumberFieldOptions {
                    min: 1.0,
                    max: u32::MAX as f64,
                    step: 1.0,
                },
                |cx: &App| cx.global::<SettingsStore>().settings.ai.page_image_limit as f64,
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.ai.page_image_limit = value as usize;
                        });
                    }
                },
            )
            .default_value(defaults.ai.page_image_limit as f64),
        )
        .description(crate::fl!("settings-page-image-limit-hint"));

        let quick_transparency = SettingItem::new(
            "Transparency",
            SettingField::number_input(
                NumberFieldOptions {
                    min: 0.0,
                    max: 1.0,
                    step: 0.05,
                },
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .ai
                        .quick_ask
                        .transparency as f64
                },
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.ai.quick_ask.transparency = value as f32;
                        });
                    }
                },
            )
            .default_value(defaults.ai.quick_ask.transparency as f64),
        );

        let quick_text_size = SettingItem::new(
            "Text size",
            SettingField::number_input(
                NumberFieldOptions {
                    min: 8.0,
                    max: 32.0,
                    step: 1.0,
                },
                |cx: &App| cx.global::<SettingsStore>().settings.ai.quick_ask.text_size as f64,
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.ai.quick_ask.text_size = value as f32;
                        });
                    }
                },
            )
            .default_value(defaults.ai.quick_ask.text_size as f64),
        );

        SettingPage::new("AI")
            .description(
                "Configure models and document context shared by Glyphst Reader's AI features.",
            )
            .groups([
                SettingGroup::new()
                    .title(crate::fl!("settings-default-models"))
                    .description(crate::fl!("settings-model-list-hint"))
                    .items([main_model, title_model, translation_model, thinking]),
                SettingGroup::new()
                    .title(crate::fl!("settings-page-context"))
                    .items([text_format, image_quality, page_image_limit]),
                SettingGroup::new()
                    .title(crate::fl!("settings-quick-ai-chat"))
                    .items([quick_transparency, quick_text_size]),
            ])
    }

    pub(super) fn provider_settings_page(&self, cx: &mut Context<Self>) -> SettingPage {
        let view = cx.entity();
        let groups = AiProvider::ALL.into_iter().map(|provider| {
            let defaults = provider.default_config();
            let enabled = SettingItem::new(
                "Enabled",
                SettingField::switch(
                    move |cx: &App| {
                        cx.global::<SettingsStore>()
                            .settings
                            .ai
                            .provider_config(provider)
                            .enabled
                    },
                    {
                        let view = view.clone();
                        move |value, cx| {
                            Self::update_settings(&view, cx, move |settings| {
                                settings.ai.provider_config_mut(provider).enabled = value;
                            });
                        }
                    },
                )
                .default_value(defaults.enabled),
            )
            .description("Make this provider and its models available to AI features.");

            let api_key_field = Self::bound_text_field(
                format!("provider-api-key-{}", provider.as_str()),
                crate::fl!("settings-api-key-placeholder"),
                true,
                move |cx| {
                    cx.global::<SettingsStore>()
                        .settings
                        .ai
                        .provider_config(provider)
                        .api_key
                        .into()
                },
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_text_settings(&view, cx, move |settings| {
                            settings.ai.provider_config_mut(provider).api_key = value.to_string();
                        });
                        view.update(cx, |this, cx| {
                            this.invalidate_provider_operation(provider);
                            cx.notify();
                        });
                    }
                },
            )
            .on_reset(
                move |cx| {
                    !cx.global::<SettingsStore>()
                        .settings
                        .ai
                        .provider_config(provider)
                        .api_key
                        .is_empty()
                },
                {
                    let view = view.clone();
                    move |_, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.ai.provider_config_mut(provider).api_key.clear();
                        });
                        view.update(cx, |this, cx| {
                            this.invalidate_provider_operation(provider);
                            cx.notify();
                        });
                    }
                },
            );
            let api_key = SettingItem::new(crate::fl!("settings-api-key"), api_key_field)
                .description("Stored locally in Glyphst Reader's settings file.");

            let base_url_field = Self::bound_text_field(
                format!("provider-base-url-{}", provider.as_str()),
                provider.default_base_url(),
                false,
                move |cx| {
                    cx.global::<SettingsStore>()
                        .settings
                        .ai
                        .provider_config(provider)
                        .base_url
                        .unwrap_or_else(|| provider.default_base_url().to_string())
                        .into()
                },
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_text_settings(&view, cx, move |settings| {
                            settings.ai.provider_config_mut(provider).base_url =
                                Some(value.to_string());
                        });
                        view.update(cx, |this, cx| {
                            this.invalidate_provider_operation(provider);
                            cx.notify();
                        });
                    }
                },
            )
            .on_reset(
                move |cx| {
                    cx.global::<SettingsStore>()
                        .settings
                        .ai
                        .provider_config(provider)
                        .base_url
                        .is_some()
                },
                {
                    let view = view.clone();
                    move |_, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.ai.provider_config_mut(provider).base_url = None;
                        });
                        view.update(cx, |this, cx| {
                            this.invalidate_provider_operation(provider);
                            cx.notify();
                        });
                    }
                },
            );
            let base_url = SettingItem::new(crate::fl!("settings-api-host"), base_url_field)
                .layout(Axis::Vertical)
                .description(format!("Default: {}", provider.default_base_url()));

            let models = SettingItem::new(
                crate::fl!("settings-model-list"),
                SettingField::render({
                    let view = view.clone();
                    move |options, window, cx| {
                        Self::provider_models_field(provider, view.clone(), options, window, cx)
                    }
                })
                .on_reset(
                    move |cx| {
                        cx.global::<SettingsStore>()
                            .settings
                            .ai
                            .provider_config(provider)
                            .models
                            != provider.default_config().models
                    },
                    {
                        let view = view.clone();
                        move |_, cx| {
                            Self::update_settings(&view, cx, move |settings| {
                                settings.ai.provider_config_mut(provider).models =
                                    provider.default_models();
                            });
                        }
                    },
                ),
            )
            .layout(Axis::Vertical)
            .description(
                "Add model IDs manually or choose from the provider's cached model catalog.",
            );

            SettingGroup::new()
                .title(provider.label())
                .items([enabled, api_key, base_url, models])
        });

        SettingPage::new(crate::fl!("settings-tab-model-providers"))
            .description(
                "Enable providers, store credentials, customize API hosts, and manage their model catalogs.",
            )
            .groups(groups)
    }
}
