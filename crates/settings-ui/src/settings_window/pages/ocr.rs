use super::super::{OcrConnectionResult, SettingsWindow};
use glyphst_settings::{
    DEFAULT_PADDLE_OCR_ENDPOINT, DEFAULT_PADDLE_OCR_MODEL, DEFAULT_UNLIMITED_OCR_ENDPOINT, OcrMode,
    OcrProvider, Settings, SettingsStore,
};
use gpui::{App, Context, Entity, SharedString, prelude::*};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::Button,
    label::Label,
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
    v_flex,
};

fn ocr_mode_hint_id(mode: OcrMode) -> &'static str {
    match mode {
        OcrMode::Always => "settings-ocr-mode-always-hint",
        OcrMode::Auto => "settings-ocr-mode-auto-hint",
        OcrMode::Off => "settings-ocr-mode-off-hint",
    }
}

fn ocr_mode_hint(mode: OcrMode) -> String {
    glyphst_settings::localize::tr(ocr_mode_hint_id(mode))
}

impl SettingsWindow {
    fn clear_ocr_status(view: &Entity<Self>, cx: &mut App) {
        view.update(cx, |this, cx| {
            this._ocr_task.take();
            this.ocr_operation = None;
            cx.notify();
        });
    }

    fn ocr_connection_item(&self, provider: OcrProvider, cx: &mut Context<Self>) -> SettingItem {
        let view = cx.entity();
        SettingItem::new(
            crate::fl!("settings-test-connection"),
            SettingField::render(move |options, _, cx| {
                let operation = view
                    .read(cx)
                    .ocr_operation
                    .as_ref()
                    .filter(|operation| operation.provider == provider)
                    .cloned();
                let loading = operation.as_ref().is_some_and(|operation| {
                    matches!(operation.result, OcrConnectionResult::Testing)
                });
                let status = operation.map(|operation| match operation.result {
                    OcrConnectionResult::Testing => (
                        crate::fl!("settings-ocr-connection-testing"),
                        cx.theme().muted_foreground,
                    ),
                    OcrConnectionResult::Succeeded => (
                        crate::fl!("settings-ocr-connection-succeeded"),
                        cx.theme().success,
                    ),
                    OcrConnectionResult::InvalidAuth => (
                        crate::fl!("settings-ocr-connection-invalid-auth"),
                        cx.theme().danger,
                    ),
                    OcrConnectionResult::Retryable => (
                        crate::fl!("settings-ocr-connection-retryable"),
                        cx.theme().danger,
                    ),
                    OcrConnectionResult::ProviderError(Some(code)) => (
                        crate::fl!("settings-ocr-connection-provider-error-code", code = code),
                        cx.theme().danger,
                    ),
                    OcrConnectionResult::ProviderError(None) => (
                        crate::fl!("settings-ocr-connection-provider-error"),
                        cx.theme().danger,
                    ),
                });
                let test_view = view.clone();
                v_flex()
                    .items_end()
                    .gap_1()
                    .child(
                        Button::new(SharedString::from(format!(
                            "test-ocr-{}",
                            provider.as_str()
                        )))
                        .label(if loading {
                            crate::fl!("settings-ocr-testing")
                        } else {
                            crate::fl!("settings-test-connection")
                        })
                        .outline()
                        .loading(loading)
                        .disabled(loading)
                        .with_size(options.size)
                        .on_click(move |_, _, cx| {
                            test_view.update(cx, |this, cx| {
                                this.test_ocr_connection(provider, cx);
                            });
                        }),
                    )
                    .when_some(status, |this, (message, color)| {
                        this.child(Label::new(message).text_xs().text_color(color))
                    })
            }),
        )
    }

    pub(super) fn ocr_settings_page(&self, cx: &mut Context<Self>) -> SettingPage {
        let view = cx.entity();
        let defaults = Settings::default();
        let current_mode = cx.global::<SettingsStore>().settings.ocr.mode;

        let mode = SettingItem::new(
            crate::fl!("settings-mode"),
            SettingField::dropdown(
                vec![
                    Self::setting_option(
                        OcrMode::Always.as_str(),
                        crate::fl!("settings-ocr-mode-always"),
                    ),
                    Self::setting_option(
                        OcrMode::Auto.as_str(),
                        crate::fl!("settings-ocr-mode-auto"),
                    ),
                    Self::setting_option(
                        OcrMode::Off.as_str(),
                        crate::fl!("settings-ocr-mode-off"),
                    ),
                ],
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .ocr
                        .mode
                        .as_str()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let mode = match value.as_ref() {
                            "auto" => OcrMode::Auto,
                            "off" => OcrMode::Off,
                            _ => OcrMode::Always,
                        };
                        Self::update_settings(&view, cx, move |settings| {
                            settings.ocr.mode = mode;
                        });
                        Self::clear_ocr_status(&view, cx);
                    }
                },
            )
            .default_value(defaults.ocr.mode.as_str().to_string()),
        )
        .description(ocr_mode_hint(current_mode));

        let provider = SettingItem::new(
            crate::fl!("settings-ocr-provider"),
            SettingField::dropdown(
                vec![
                    Self::setting_option(
                        OcrProvider::Unlimited.as_str(),
                        crate::fl!("settings-ocr-unlimited"),
                    ),
                    Self::setting_option(
                        OcrProvider::Paddle.as_str(),
                        crate::fl!("settings-ocr-paddle"),
                    ),
                ],
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .ocr
                        .provider
                        .as_str()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let provider = if value.as_ref() == "paddle" {
                            OcrProvider::Paddle
                        } else {
                            OcrProvider::Unlimited
                        };
                        Self::update_settings(&view, cx, move |settings| {
                            settings.ocr.provider = provider;
                        });
                        Self::clear_ocr_status(&view, cx);
                    }
                },
            )
            .default_value(defaults.ocr.provider.as_str().to_string()),
        );

        let unlimited_api_key = SettingItem::new(
            crate::fl!("settings-ocr-unlimited-api-key"),
            Self::bound_text_field(
                "ocr-unlimited-api-key",
                crate::fl!("settings-ocr-unlimited-api-key-placeholder"),
                true,
                |cx| {
                    cx.global::<SettingsStore>()
                        .settings
                        .ocr
                        .unlimited
                        .api_key
                        .clone()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_text_settings(&view, cx, move |settings| {
                            settings.ocr.unlimited.api_key = value.to_string();
                        });
                        Self::clear_ocr_status(&view, cx);
                    }
                },
            )
            .on_reset(
                |cx| {
                    !cx.global::<SettingsStore>()
                        .settings
                        .ocr
                        .unlimited
                        .api_key
                        .is_empty()
                },
                {
                    let view = view.clone();
                    move |_, cx| {
                        Self::update_settings(&view, cx, |settings| {
                            settings.ocr.unlimited.api_key.clear();
                        });
                        Self::clear_ocr_status(&view, cx);
                    }
                },
            ),
        );

        let unlimited_secret_key = SettingItem::new(
            crate::fl!("settings-ocr-unlimited-secret-key"),
            Self::bound_text_field(
                "ocr-unlimited-secret-key",
                crate::fl!("settings-ocr-unlimited-secret-key-placeholder"),
                true,
                |cx| {
                    cx.global::<SettingsStore>()
                        .settings
                        .ocr
                        .unlimited
                        .secret_key
                        .clone()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_text_settings(&view, cx, move |settings| {
                            settings.ocr.unlimited.secret_key = value.to_string();
                        });
                        Self::clear_ocr_status(&view, cx);
                    }
                },
            )
            .on_reset(
                |cx| {
                    !cx.global::<SettingsStore>()
                        .settings
                        .ocr
                        .unlimited
                        .secret_key
                        .is_empty()
                },
                {
                    let view = view.clone();
                    move |_, cx| {
                        Self::update_settings(&view, cx, |settings| {
                            settings.ocr.unlimited.secret_key.clear();
                        });
                        Self::clear_ocr_status(&view, cx);
                    }
                },
            ),
        );

        let unlimited_endpoint = SettingItem::new(
            crate::fl!("settings-ocr-endpoint"),
            Self::bound_text_field(
                "ocr-unlimited-endpoint",
                DEFAULT_UNLIMITED_OCR_ENDPOINT,
                false,
                |cx| {
                    cx.global::<SettingsStore>()
                        .settings
                        .ocr
                        .unlimited
                        .endpoint
                        .clone()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_text_settings(&view, cx, move |settings| {
                            settings.ocr.unlimited.endpoint = value.to_string();
                        });
                        Self::clear_ocr_status(&view, cx);
                    }
                },
            )
            .on_reset(
                |cx| {
                    cx.global::<SettingsStore>().settings.ocr.unlimited.endpoint
                        != DEFAULT_UNLIMITED_OCR_ENDPOINT
                },
                {
                    let view = view.clone();
                    move |_, cx| {
                        Self::update_settings(&view, cx, |settings| {
                            settings.ocr.unlimited.endpoint =
                                DEFAULT_UNLIMITED_OCR_ENDPOINT.to_string();
                        });
                        Self::clear_ocr_status(&view, cx);
                    }
                },
            ),
        );

        let paddle_token = SettingItem::new(
            crate::fl!("settings-ocr-paddle-token"),
            Self::bound_text_field(
                "ocr-paddle-token",
                crate::fl!("settings-ocr-paddle-token-placeholder"),
                true,
                |cx| {
                    cx.global::<SettingsStore>()
                        .settings
                        .ocr
                        .paddle
                        .token
                        .clone()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_text_settings(&view, cx, move |settings| {
                            settings.ocr.paddle.token = value.to_string();
                        });
                        Self::clear_ocr_status(&view, cx);
                    }
                },
            )
            .on_reset(
                |cx| {
                    !cx.global::<SettingsStore>()
                        .settings
                        .ocr
                        .paddle
                        .token
                        .is_empty()
                },
                {
                    let view = view.clone();
                    move |_, cx| {
                        Self::update_settings(&view, cx, |settings| {
                            settings.ocr.paddle.token.clear();
                        });
                        Self::clear_ocr_status(&view, cx);
                    }
                },
            ),
        );

        let paddle_endpoint = SettingItem::new(
            crate::fl!("settings-ocr-endpoint"),
            Self::bound_text_field(
                "ocr-paddle-endpoint",
                DEFAULT_PADDLE_OCR_ENDPOINT,
                false,
                |cx| {
                    cx.global::<SettingsStore>()
                        .settings
                        .ocr
                        .paddle
                        .endpoint
                        .clone()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_text_settings(&view, cx, move |settings| {
                            settings.ocr.paddle.endpoint = value.to_string();
                        });
                        Self::clear_ocr_status(&view, cx);
                    }
                },
            )
            .on_reset(
                |cx| {
                    cx.global::<SettingsStore>().settings.ocr.paddle.endpoint
                        != DEFAULT_PADDLE_OCR_ENDPOINT
                },
                {
                    let view = view.clone();
                    move |_, cx| {
                        Self::update_settings(&view, cx, |settings| {
                            settings.ocr.paddle.endpoint = DEFAULT_PADDLE_OCR_ENDPOINT.to_string();
                        });
                        Self::clear_ocr_status(&view, cx);
                    }
                },
            ),
        );

        let paddle_model = SettingItem::new(
            crate::fl!("settings-ocr-model"),
            Self::bound_text_field(
                "ocr-paddle-model",
                DEFAULT_PADDLE_OCR_MODEL,
                false,
                |cx| {
                    cx.global::<SettingsStore>()
                        .settings
                        .ocr
                        .paddle
                        .model
                        .clone()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_text_settings(&view, cx, move |settings| {
                            settings.ocr.paddle.model = value.to_string();
                        });
                        Self::clear_ocr_status(&view, cx);
                    }
                },
            )
            .on_reset(
                |cx| {
                    cx.global::<SettingsStore>().settings.ocr.paddle.model
                        != DEFAULT_PADDLE_OCR_MODEL
                },
                {
                    let view = view.clone();
                    move |_, cx| {
                        Self::update_settings(&view, cx, |settings| {
                            settings.ocr.paddle.model = DEFAULT_PADDLE_OCR_MODEL.to_string();
                        });
                        Self::clear_ocr_status(&view, cx);
                    }
                },
            ),
        );

        let unlimited_test = self.ocr_connection_item(OcrProvider::Unlimited, cx);
        let paddle_test = self.ocr_connection_item(OcrProvider::Paddle, cx);

        SettingPage::new(crate::fl!("settings-tab-ocr")).groups([
            SettingGroup::new()
                .title(crate::fl!("settings-ocr-behavior"))
                .items([mode, provider]),
            SettingGroup::new()
                .title(crate::fl!("settings-ocr-unlimited"))
                .items([
                    unlimited_api_key,
                    unlimited_secret_key,
                    unlimited_endpoint,
                    unlimited_test,
                ]),
            SettingGroup::new()
                .title(crate::fl!("settings-ocr-paddle"))
                .items([paddle_token, paddle_endpoint, paddle_model, paddle_test]),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::ocr_mode_hint_id;
    use glyphst_settings::OcrMode;

    #[test]
    fn mode_hint_matches_the_selected_mode() {
        assert_eq!(
            ocr_mode_hint_id(OcrMode::Always),
            "settings-ocr-mode-always-hint"
        );
        assert_eq!(
            ocr_mode_hint_id(OcrMode::Auto),
            "settings-ocr-mode-auto-hint"
        );
        assert_eq!(ocr_mode_hint_id(OcrMode::Off), "settings-ocr-mode-off-hint");
    }
}
